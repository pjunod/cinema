from __future__ import annotations

import contextlib
import dataclasses
import io
import json
import os
from pathlib import Path
import shlex
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock
import xml.etree.ElementTree as ET

from validation import ci_scope, runner as validation_runner
from validation.ci_scope import (
    CI_ROUTING_PATHS,
    all_scope,
    catalog_is_the_only_routing_edit,
    is_docs_only,
    needs_rust_gate,
    resolve_scope,
    scope_for_paths,
)
from validation.runner import (
    CatalogError,
    CheckResult,
    changed_paths,
    execute_checks,
    glob_regex,
    lint_catalog,
    load_catalog,
    matches,
    select_named,
    select_points,
    selected_checks,
    write_reports,
)


ROOT = Path(__file__).resolve().parents[2]


CATALOG = """
version = 1

[settings]
profiles = ["commit", "full"]
always_checks = ["baseline"]

[[checks]]
id = "baseline"
title = "Baseline"
command = "true"
profiles = ["commit", "full"]

[[checks]]
id = "browser"
title = "Browser"
command = "true"
profiles = ["full"]

[[points]]
id = "core"
title = "Core"
contract = "Core stays true."
paths = ["src/**"]
checks = ["baseline"]

[[points]]
id = "web"
title = "Web"
contract = "Web stays true."
paths = ["web/**/*.js"]
checks = ["browser", "baseline"]
depends_on = ["core"]
"""


class CatalogCase(unittest.TestCase):
    def load(self, content: str = CATALOG):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        path = Path(temporary.name) / "points.toml"
        path.write_text(content, encoding="utf-8")
        return load_catalog(path)

    def test_double_star_matches_zero_or_many_directories(self):
        pattern = glob_regex("web/**/*.js")
        self.assertIsNotNone(pattern.match("web/app.js"))
        self.assertIsNotNone(pattern.match("web/player/app.js"))
        self.assertIsNone(pattern.match("web/player/app.css"))

    def test_cluster_auth_budget_covers_a_cold_serial_run(self):
        catalog = load_catalog(ROOT / "validation/points.toml")
        cluster_auth = next(
            check for check in catalog.checks if check.id == "cluster-auth"
        )

        self.assertEqual(cluster_auth.timeout_seconds, 3600)

    def test_gitignore_changes_select_the_validation_framework(self):
        catalog = load_catalog(ROOT / "validation/points.toml")
        selection = select_points(catalog, (".gitignore",))
        check_ids = {
            check.id for check in selected_checks(catalog, selection, profile="full")
        }

        self.assertIn("validation.framework", selection.point_ids)
        self.assertIn("catalog-contract", check_ids)

    def test_provider_change_expands_consumers_and_deduplicates_checks(self):
        catalog = self.load()
        selection = select_points(catalog, ("src/domain.py",))
        self.assertEqual(selection.point_ids, ("core", "web"))
        self.assertEqual(selection.reasons["web"], ("consumer:core",))
        self.assertEqual(
            tuple(check.id for check in selected_checks(catalog, selection, "full")),
            ("baseline", "browser"),
        )

    def test_consumer_change_does_not_point_upstream(self):
        catalog = self.load()
        selection = select_points(catalog, ("web/player/app.js",))
        self.assertEqual(selection.point_ids, ("web",))

    def test_apple_ui_test_change_does_not_select_web_layout(self):
        catalog = load_catalog(ROOT / "validation/points.toml")
        selection = select_points(
            catalog, ("clients/apple/Tests/AppleClientTests.swift",)
        )
        check_ids = {
            check.id for check in selected_checks(catalog, selection, profile="ci")
        }

        self.assertIn("apple-simulators", check_ids)
        self.assertNotIn("web-layout", check_ids)

    def test_the_browser_gate_selects_the_only_job_that_has_a_browser(self):
        """A gate that never runs is worse than no gate.

        `scripts/control-reporter-browser-check` needs Playwright and a
        Chromium, which exist in exactly one job — the one `web_layout`
        selects. It shipped with a `validation/points.toml` entry and without a
        `WEB_LAYOUT_PATHS` entry, so its own pull request skipped the job
        entirely and the gate proved nothing about itself.

        The reporter's own path matters more: a change to
        `crates/plurxd/src/web/**` has to select the browser job, because that
        is the diff this gate exists to catch.
        """
        catalog = load_catalog(ROOT / "validation/points.toml")

        gate = scope_for_paths(catalog, ("scripts/control-reporter-browser-check",))
        self.assertTrue(gate["web_layout"])

        reporter = scope_for_paths(
            catalog, ("crates/plurxd/src/web/playback-control.js",)
        )
        self.assertTrue(reporter["web_layout"])

    def test_ci_scope_keeps_expensive_jobs_on_their_affected_surfaces(self):
        catalog = load_catalog(ROOT / "validation/points.toml")

        operations = scope_for_paths(
            catalog, ("scripts/ship", "tests/operations/test_contracts.py")
        )
        # Scripts feed packaging and selection behavior the cargo suite pins,
        # so the Rust lane stays on — but no client or container surface does.
        self.assertTrue(operations["rust"])
        self.assertFalse(
            any(value for key, value in operations.items() if key != "rust")
        )

        android = scope_for_paths(
            catalog,
            ("clients/android/app/src/main/java/tv/plurx/app/player/PlayerScreen.kt",),
        )
        self.assertTrue(android["android_jvm"])
        self.assertTrue(android["android_device"])
        self.assertFalse(android["rust"])
        self.assertFalse(android["apple"])
        self.assertFalse(android["web_layout"])
        self.assertFalse(android["release_build"])
        self.assertFalse(android["container"])

        # The pull-request lane defers cross-builds and container rebuilds to
        # the merge queue: ordinary server code runs the fast Rust lane only.
        server = scope_for_paths(catalog, ("crates/plurxd/src/http/stream.rs",))
        self.assertTrue(server["rust"])
        self.assertFalse(server["release_build"])
        self.assertFalse(server["container"])
        self.assertFalse(server["apple"])
        self.assertFalse(server["android_jvm"])
        self.assertFalse(server["web_layout"])
        self.assertFalse(server["android_device"])
        self.assertFalse(server["hiqlite_spike"])
        self.assertFalse(server["cluster_auth"])

        packaging = scope_for_paths(catalog, ("Cargo.toml", "Dockerfile"))
        self.assertTrue(packaging["release_build"])
        self.assertTrue(packaging["container"])
        for build_only_path in (
            ".github/actions/buildx-cache/action.yml",
            ".github/buildkitd.toml",
            "crates/plurxd/build.rs",
            "scripts/ci-buildkit-prune",
            "scripts/ci-execution-mode",
            "vendor/hiqlite/Cargo.toml",
            "validation/ci_scope.py",
            "validation/release_dockerfile.py",
            "tests/operations/test_release_publication.py",
        ):
            with self.subTest(build_only_path=build_only_path):
                self.assertTrue(
                    scope_for_paths(catalog, (build_only_path,))["release_build"]
                )
        unknown = scope_for_paths(catalog, ("unclassified/build-input.xyz",))
        self.assertTrue(unknown["release_build"])

        web = scope_for_paths(catalog, ("crates/plurxd/src/web/app.js",))
        self.assertTrue(web["rust"])
        self.assertTrue(web["web_layout"])
        self.assertFalse(web["hiqlite_spike"])
        self.assertFalse(web["cluster_auth"])

        for page_read_path in (
            "crates/plurxd/src/http/browse.rs",
            "crates/plurxd/src/http/system.rs",
        ):
            with self.subTest(page_read_path=page_read_path):
                page_read = scope_for_paths(catalog, (page_read_path,))
                self.assertTrue(page_read["cluster_auth"])

        for web_only_path in (
            "Makefile",
            "crates/plurxd/src/web/index.html",
            "tests/web/page-read-budget.test.js",
        ):
            with self.subTest(web_only_path=web_only_path):
                web_only = scope_for_paths(catalog, (web_only_path,))
                self.assertFalse(web_only["cluster_auth"])

        cluster = scope_for_paths(
            catalog, ("crates/plurx-core/src/store/hiqlite.rs",)
        )
        self.assertFalse(cluster["hiqlite_spike"])
        self.assertTrue(cluster["cluster_auth"])

        for store_shard_path in (
            "Dockerfile.store-shard",
            "validation/store_shard.py",
            "tests/validation/test_store_shard.py",
        ):
            with self.subTest(store_shard_path=store_shard_path):
                shard_scope = scope_for_paths(catalog, (store_shard_path,))
                self.assertTrue(shard_scope["rust"])
                self.assertTrue(shard_scope["cluster_auth"])

        topology_schema = scope_for_paths(
            catalog, ("benchmarks/cluster-topology.schema.json",)
        )
        self.assertTrue(topology_schema["cluster_auth"])

        # A membership change is only exercised against real voters in the
        # replicated lane, so it must select it.
        membership = scope_for_paths(
            catalog, ("crates/plurx-core/src/cluster/membership.rs",)
        )
        self.assertTrue(membership["cluster_auth"])

        core = scope_for_paths(catalog, ("crates/plurx-core/src/domain.rs",))
        self.assertTrue(core["hiqlite_spike"])
        self.assertTrue(core["cluster_auth"])

        cluster_selection = select_points(
            catalog, ("crates/plurx-core/src/store/hiqlite.rs",)
        )
        cluster_ci_checks = {
            check.id
            for check in selected_checks(catalog, cluster_selection, profile="ci")
        }
        self.assertNotIn("cluster-auth", cluster_ci_checks)

        fallback = all_scope()
        executable_scope = (
            value for key, value in fallback.items() if key != "docs_only"
        )
        self.assertTrue(all(executable_scope))
        self.assertFalse(fallback["docs_only"])

    def test_client_only_diffs_skip_the_cargo_lane_but_keep_their_own(self):
        catalog = load_catalog(ROOT / "validation/points.toml")

        apple = scope_for_paths(
            catalog, ("clients/apple/Sources/PlayerSurface.swift",)
        )
        self.assertFalse(apple["rust"])
        self.assertTrue(apple["apple"])
        self.assertTrue(apple["mobile_version"])
        self.assertFalse(apple["android_jvm"])
        self.assertFalse(apple["android_device"])
        self.assertFalse(apple["web_layout"])
        self.assertFalse(apple["release_build"])
        self.assertFalse(apple["container"])

        # A Kotlin diff plus its own release notes is still client-only.
        mixed = scope_for_paths(
            catalog,
            (
                "clients/android/app/src/main/java/tv/plurx/app/MainActivity.kt",
                "clients/android/README.md",
                "docs/FEATURES.md",
            ),
        )
        self.assertFalse(mixed["rust"])
        self.assertTrue(mixed["android_jvm"])

        # The one file that compiles into BOTH native clients: editing the
        # shared wire fixture must re-run both client suites on the PR itself,
        # and it lives under tests/, so the Rust lane runs too.
        fixture = scope_for_paths(catalog, ("tests/contracts/native-api.json",))
        self.assertTrue(fixture["rust"])
        self.assertTrue(fixture["apple"])
        self.assertTrue(fixture["android_jvm"])

        self.assertTrue(needs_rust_gate(()))

    def test_qualification_and_release_events_fail_open_into_the_full_fan_out(self):
        for event in ("effort_qualification", "merge_group", "push"):
            with self.subTest(event=event):
                self.assertEqual(resolve_scope(event, None), all_scope())

    def test_ci_profile_splits_the_rust_gate_and_local_profiles_keep_it_whole(self):
        catalog = load_catalog(ROOT / "validation/points.toml")
        self.assertEqual(
            set(catalog.always_checks),
            {"catalog-contract", "history-regressions"},
        )
        selection = select_points(catalog, ("crates/plurxd/src/http/stream.rs",))

        ci = {check.id for check in selected_checks(catalog, selection, "ci")}
        self.assertIn("rust-gate-ci", ci)
        self.assertNotIn("rust-gate", ci)
        # These re-run binaries rust-gate-ci already executes with identical
        # feature resolution; in CI they would be pure duplication.
        self.assertNotIn("api-wire", ci)
        self.assertNotIn("security-boundaries", ci)
        self.assertNotIn("user-journey", ci)

        commit = {check.id for check in selected_checks(catalog, selection, "commit")}
        self.assertIn("rust-gate", commit)
        self.assertNotIn("rust-gate-ci", commit)

        gate = catalog.check_map["rust-gate-ci"]
        self.assertEqual(gate.command, "make ci-rust-gate")

    def test_ci_scope_routes_documentation_only_diffs_to_fast_preflight(self):
        catalog = load_catalog(ROOT / "validation/points.toml")

        scope = scope_for_paths(
            catalog,
            (
                "docs/OFFLINE-VIEWING-PLAN.md",
                "docs/OFFLINE-VIEWING-REVIEW.md",
                "docs/STATUS.html",
                "validation/regressions.d/a828a217-playback-pipeline.toml",
            ),
        )

        self.assertTrue(scope["docs_only"])
        executable_scope = (
            value for key, value in scope.items() if key != "docs_only"
        )
        self.assertFalse(any(executable_scope))
        self.assertTrue(is_docs_only((".github/PULL_REQUEST_TEMPLATE.md",)))
        self.assertFalse(is_docs_only(("clients/apple/Sources/Notes.md",)))
        self.assertFalse(is_docs_only(("crates/plurx-core/README.md",)))

    def test_ci_scope_does_not_treat_selector_changes_as_documentation(self):
        catalog = load_catalog(ROOT / "validation/points.toml")

        scope = scope_for_paths(
            catalog,
            ("docs/PLAYBACK.md", "validation/points.toml"),
        )

        self.assertFalse(scope["docs_only"])

        for scheduler_path in (
            ".github/workflows/ci.yml",
            ".github/workflows/effort-ci.yml",
            ".github/workflows/lint.yml",
            "validation/ci_scope.py",
            "validation/points.toml",
            "validation/runner.py",
        ):
            with self.subTest(scheduler_path=scheduler_path):
                scheduler_scope = scope_for_paths(catalog, (scheduler_path,))
                expected = {"rust", "cluster_auth"}
                if scheduler_path in (
                    ".github/workflows/ci.yml",
                    "validation/ci_scope.py",
                ):
                    expected.add("release_build")
                self.assertEqual(
                    {key for key, value in scheduler_scope.items() if value},
                    expected,
                )
                self.assertFalse(scheduler_scope["docs_only"])

        ffmpeg = scope_for_paths(
            catalog, (".github/actions/ffmpeg/action.yml",)
        )
        self.assertEqual(
            {key for key, value in ffmpeg.items() if value},
            {"rust", "web_layout", "cluster_auth"},
        )

    def test_ci_scope_fails_open_for_mixed_documentation_and_code(self):
        catalog = load_catalog(ROOT / "validation/points.toml")

        scope = scope_for_paths(
            catalog,
            ("docs/PLAYBACK.md", "crates/plurxd/src/http/stream.rs"),
        )

        self.assertFalse(scope["docs_only"])
        self.assertTrue(scope["rust"])
        self.assertFalse(scope["release_build"])
        self.assertFalse(scope["container"])

    def test_profile_keeps_mandatory_baseline_when_slow_check_is_ineligible(self):
        catalog = self.load()
        selection = select_named(catalog, ("web",))
        self.assertEqual(
            tuple(check.id for check in selected_checks(catalog, selection, "commit")),
            ("baseline",),
        )

    def test_lint_rejects_unknown_check(self):
        catalog = self.load(CATALOG.replace('checks = ["browser", "baseline"]', 'checks = ["missing"]'))
        errors = lint_catalog(catalog, audit=False)
        self.assertIn("point web references unknown check missing", errors)

    def test_lint_rejects_duplicate_ids_and_dependency_cycles(self):
        duplicate = self.load(CATALOG + """

[[points]]
id = "core"
title = "Duplicate core"
contract = "This must be rejected."
paths = ["duplicate/**"]
checks = ["baseline"]
""")
        self.assertIn("duplicate point id: core", lint_catalog(duplicate, audit=False))

        cycle_text = CATALOG.replace(
            'paths = ["src/**"]\nchecks = ["baseline"]',
            'paths = ["src/**"]\nchecks = ["baseline"]\ndepends_on = ["web"]',
        )
        cycle = self.load(cycle_text)
        self.assertTrue(
            any(error.startswith("point dependency cycle:") for error in lint_catalog(cycle, audit=False))
        )

    def test_unknown_named_point_is_an_error(self):
        catalog = self.load()
        with self.assertRaises(CatalogError):
            select_named(catalog, ("missing",))

    def test_repository_globs_do_not_let_single_star_cross_directories(self):
        self.assertTrue(matches("crates/core/src/lib.rs", ("crates/**",)))
        self.assertFalse(matches("crates/core/src/lib.rs", ("crates/*.rs",)))

    def test_braces_keep_related_path_triggers_reviewable(self):
        pattern = ("scripts/{bench,ship}",)
        self.assertTrue(matches("scripts/bench", pattern))
        self.assertTrue(matches("scripts/ship", pattern))
        self.assertFalse(matches("scripts/validate", pattern))

    def test_lint_rejects_a_literal_path_that_does_not_resolve(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "present.rs").write_text("// present\n", encoding="utf-8")
            catalog = self.load(
                CATALOG.replace(
                    'paths = ["src/**"]',
                    'paths = ["present.rs", "missing.rs"]',
                    1,
                )
            )
            errors = lint_catalog(catalog, repo_root=root, audit=False)

        self.assertIn(
            "point core references missing literal path 'missing.rs'",
            errors,
        )
        self.assertNotIn(
            "point core references missing literal path 'present.rs'",
            errors,
        )

    def test_lint_rejects_a_glob_that_matches_no_tracked_file(self):
        catalog = self.load(
            CATALOG.replace('paths = ["web/**/*.js"]', 'paths = ["renamed/**/*.js"]')
        )
        errors = lint_catalog(
            catalog,
            audit=False,
            tracked_paths=("src/lib.rs", "web/app.js"),
        )

        self.assertIn(
            "point web path glob matches no tracked file: 'renamed/**/*.js'",
            errors,
        )

    def test_forward_looking_glob_needs_an_exact_allowlist_entry(self):
        catalog = self.load(
            CATALOG.replace(
                'always_checks = ["baseline"]',
                'always_checks = ["baseline"]\nallow_unmatched_globs = ["future/**/*.md"]',
            ).replace('paths = ["web/**/*.js"]', 'paths = ["future/**/*.md"]')
        )
        errors = lint_catalog(
            catalog,
            audit=False,
            tracked_paths=("src/lib.rs",),
        )

        self.assertNotIn(
            "point web path glob matches no tracked file: 'future/**/*.md'",
            errors,
        )

    def test_missing_optional_prerequisite_is_visible_and_strict_can_fail_it(self):
        catalog = self.load()
        check = dataclasses.replace(
            catalog.check_map["browser"],
            requires_files=("does-not-exist",),
            missing="skip",
        )
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with contextlib.redirect_stdout(io.StringIO()):
                optional = execute_checks(
                    (check,), root, root / "artifacts", strict=False, fail_fast=False
                )
                strict = execute_checks(
                    (check,), root, root / "artifacts", strict=True, fail_fast=False
                )
        self.assertEqual(optional[0].status, "skipped")
        self.assertEqual(strict[0].status, "failed")
        self.assertIn("missing files", optional[0].message)

    def test_staged_path_resolution_handles_added_and_renamed_files(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            subprocess.run(["git", "init", "-q"], cwd=root, check=True)
            subprocess.run(["git", "config", "user.name", "Validation Test"], cwd=root, check=True)
            subprocess.run(["git", "config", "user.email", "validation@example.invalid"], cwd=root, check=True)
            original = root / "old name.txt"
            original.write_text("old\n", encoding="utf-8")
            deleted = root / "deleted.txt"
            deleted.write_text("delete me\n", encoding="utf-8")
            subprocess.run(["git", "add", "old name.txt"], cwd=root, check=True)
            subprocess.run(["git", "add", "deleted.txt"], cwd=root, check=True)
            subprocess.run(["git", "commit", "-qm", "seed"], cwd=root, check=True)
            original.rename(root / "new name.txt")
            deleted.unlink()
            (root / "added.txt").write_text("new\n", encoding="utf-8")
            subprocess.run(["git", "add", "-A"], cwd=root, check=True)

            self.assertEqual(
                changed_paths(root, "staged"),
                ("added.txt", "deleted.txt", "new name.txt"),
            )

    def test_timeout_and_fail_fast_are_recorded_as_failures(self):
        catalog = self.load()
        timeout = dataclasses.replace(
            catalog.check_map["baseline"],
            command="python3 -c 'import time; time.sleep(2)'",
            timeout_seconds=1,
        )
        never = catalog.check_map["browser"]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with contextlib.redirect_stdout(io.StringIO()):
                results = execute_checks(
                    (timeout, never), root, root / "artifacts", strict=True, fail_fast=True
                )
        self.assertEqual(len(results), 1)
        self.assertEqual(results[0].status, "failed")
        self.assertEqual(results[0].returncode, 124)
        self.assertIn("timed out", results[0].output)

    @unittest.skipIf(os.name == "nt", "POSIX process census contract")
    def test_process_census_failure_refuses_to_launch_a_check(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            marker = root / "launched"
            with (
                mock.patch.object(
                    validation_runner,
                    "_process_snapshot",
                    side_effect=PermissionError("injected preflight denial"),
                ),
                self.assertRaisesRegex(PermissionError, "injected preflight denial"),
            ):
                validation_runner._run_shell(
                    f"printf launched > {shlex.quote(str(marker))}", root, 1
                )

            self.assertFalse(marker.exists())

    @unittest.skipIf(os.name == "nt", "POSIX process-group contract")
    def test_timeout_terminates_descendants_across_sessions(self):
        catalog = self.load()
        for isolation in ("setpgrp", "setsid"):
            with (
                self.subTest(isolation=isolation),
                tempfile.TemporaryDirectory() as directory,
            ):
                root = Path(directory)
                ready = root / "ready"
                marker = root / "survived"
                parent = root / "parent.py"
                parent.write_text(
                    """\
from pathlib import Path
import subprocess
import sys
import time

isolation = sys.argv[1]
ready = sys.argv[2]
marker = sys.argv[3]
subprocess.Popen([
    sys.executable,
    "-c",
    "from pathlib import Path; import os, signal, sys, time; "
    "os.setpgrp() if sys.argv[1] == 'setpgrp' else os.setsid(); "
    "signal.signal(signal.SIGTERM, signal.SIG_IGN); "
    "Path(sys.argv[2]).write_text('ready', encoding='utf-8'); time.sleep(1.5); "
    "Path(sys.argv[3]).write_text('survived', encoding='utf-8')",
    isolation,
    ready,
    marker,
])
deadline = time.monotonic() + 5
while not Path(ready).exists() and time.monotonic() < deadline:
    time.sleep(0.01)
if not Path(ready).exists():
    raise RuntimeError("grandchild did not enter its independent session")
time.sleep(30)
""",
                    encoding="utf-8",
                )
                timed_tree = dataclasses.replace(
                    catalog.check_map["baseline"],
                    command=f"{shlex.quote(sys.executable)} {shlex.quote(str(parent))} "
                    f"{isolation} {shlex.quote(str(ready))} {shlex.quote(str(marker))}",
                    timeout_seconds=1,
                )
                with contextlib.redirect_stdout(io.StringIO()):
                    results = execute_checks(
                        (timed_tree,),
                        root,
                        root / "artifacts",
                        strict=True,
                        fail_fast=True,
                    )

                self.assertEqual(results[0].returncode, 124)
                self.assertTrue(ready.exists())
                wait_after_ready = 1.75 - (time.time() - ready.stat().st_mtime)
                if wait_after_ready > 0:
                    time.sleep(wait_after_ready)
                self.assertFalse(marker.exists())

    @unittest.skipIf(os.name == "nt", "POSIX session ownership contract")
    def test_timeout_terminates_reparented_group_in_launch_session(self):
        catalog = self.load()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            ready = root / "ready"
            marker = root / "survived"
            intermediate = root / "intermediate.py"
            intermediate.write_text(
                """\
from pathlib import Path
import os
import signal
import sys
import time

os.setpgrp()
if os.fork() != 0:
    os._exit(0)
signal.signal(signal.SIGTERM, signal.SIG_IGN)
ready = Path(sys.argv[1])
marker = Path(sys.argv[2])
ready.write_text("ready", encoding="utf-8")
time.sleep(1.5)
marker.write_text("survived", encoding="utf-8")
""",
                encoding="utf-8",
            )
            parent = root / "parent.py"
            parent.write_text(
                """\
from pathlib import Path
import subprocess
import sys
import time

subprocess.Popen([sys.executable, sys.argv[1], sys.argv[2], sys.argv[3]])
deadline = time.monotonic() + 5
while not Path(sys.argv[2]).exists() and time.monotonic() < deadline:
    time.sleep(0.01)
if not Path(sys.argv[2]).exists():
    raise RuntimeError("reparented group did not become ready")
time.sleep(30)
""",
                encoding="utf-8",
            )
            timed_tree = dataclasses.replace(
                catalog.check_map["baseline"],
                command=(
                    f"{shlex.quote(sys.executable)} {shlex.quote(str(parent))} "
                    f"{shlex.quote(str(intermediate))} {shlex.quote(str(ready))} "
                    f"{shlex.quote(str(marker))}"
                ),
                timeout_seconds=1,
            )
            with contextlib.redirect_stdout(io.StringIO()):
                results = execute_checks(
                    (timed_tree,),
                    root,
                    root / "artifacts",
                    strict=True,
                    fail_fast=True,
                )

            self.assertEqual(results[0].returncode, 124)
            self.assertTrue(ready.exists())
            wait_after_ready = 1.75 - (time.time() - ready.stat().st_mtime)
            if wait_after_ready > 0:
                time.sleep(wait_after_ready)
            self.assertFalse(marker.exists())

    @unittest.skipIf(os.name == "nt", "POSIX launch-session ownership contract")
    def test_timeout_cleans_launch_session_after_shell_leader_exits(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            ready = root / "ready"
            marker = root / "survived"
            orphan = root / "orphan.py"
            orphan.write_text(
                """\
from pathlib import Path
import os
import signal
import sys
import time

if os.fork() != 0:
    os._exit(0)
os.setpgrp()
signal.signal(signal.SIGTERM, signal.SIG_IGN)
ready = Path(sys.argv[1])
marker = Path(sys.argv[2])
ready.write_text("ready", encoding="utf-8")
time.sleep(1.5)
marker.write_text("survived", encoding="utf-8")
""",
                encoding="utf-8",
            )

            returncode, output, _ = validation_runner._run_shell(
                f"{shlex.quote(sys.executable)} {shlex.quote(str(orphan))} "
                f"{shlex.quote(str(ready))} {shlex.quote(str(marker))}",
                root,
                1,
            )

            self.assertEqual(returncode, 124, output)
            self.assertTrue(ready.exists())
            wait_after_ready = 1.75 - (time.time() - ready.stat().st_mtime)
            if wait_after_ready > 0:
                time.sleep(wait_after_ready)
            self.assertFalse(marker.exists())

    @unittest.skipIf(os.name == "nt", "POSIX process cleanup contract")
    def test_timeout_cleanup_failure_kills_root_group_and_aborts(self):
        catalog = self.load()
        for cleanup_snapshots_before_failure in (0, 1):
            with (
                self.subTest(
                    cleanup_snapshots_before_failure=cleanup_snapshots_before_failure
                ),
                tempfile.TemporaryDirectory() as directory,
            ):
                root = Path(directory)
                marker = root / "survived"
                timed_tree = dataclasses.replace(
                    catalog.check_map["baseline"],
                    command=(
                        f"sleep 1.5; printf survived > {shlex.quote(str(marker))}"
                    ),
                    timeout_seconds=1,
                )
                original_snapshot = validation_runner._process_snapshot
                calls = 0

                def fail_discovery(
                    deadline: float,
                ) -> dict[int, validation_runner._ProcessRecord]:
                    nonlocal calls
                    calls += 1
                    if calls <= 1 + cleanup_snapshots_before_failure:
                        return original_snapshot(deadline)
                    raise PermissionError("injected process discovery denial")

                with (
                    mock.patch.object(
                        validation_runner,
                        "_process_snapshot",
                        side_effect=fail_discovery,
                    ),
                    contextlib.redirect_stdout(io.StringIO()),
                    self.assertRaisesRegex(
                        RuntimeError,
                        "could not prove the attached validation process tree",
                    ),
                ):
                    execute_checks(
                        (timed_tree,),
                        root,
                        root / "artifacts",
                        strict=True,
                        fail_fast=True,
                    )

                time.sleep(0.75)
                self.assertFalse(marker.exists())

    @unittest.skipIf(os.name == "nt", "POSIX process cleanup contract")
    def test_timeout_never_resumes_a_term_handler(self):
        catalog = self.load()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            ready = root / "ready"
            marker = root / "term-handler-ran"
            child = root / "term_child.py"
            child.write_text(
                """\
from pathlib import Path
import signal
import sys
import time

marker = Path(sys.argv[1])
ready = Path(sys.argv[2])
signal.signal(signal.SIGTERM, lambda *_: marker.write_text("ran", encoding="utf-8"))
ready.write_text("ready", encoding="utf-8")
time.sleep(30)
""",
                encoding="utf-8",
            )
            timed_tree = dataclasses.replace(
                catalog.check_map["baseline"],
                command=(
                    f"{shlex.quote(sys.executable)} {shlex.quote(str(child))} "
                    f"{shlex.quote(str(marker))} {shlex.quote(str(ready))}"
                ),
                timeout_seconds=1,
            )
            with contextlib.redirect_stdout(io.StringIO()):
                results = execute_checks(
                    (timed_tree,),
                    root,
                    root / "artifacts",
                    strict=True,
                    fail_fast=True,
                )

            self.assertEqual(results[0].returncode, 124)
            self.assertTrue(ready.exists())
            self.assertFalse(marker.exists())

    @unittest.skipIf(os.name == "nt", "POSIX process cleanup contract")
    def test_process_tree_freeze_requires_a_fixed_point(self):
        calls = 0

        def growing_snapshot(
            _deadline: float,
            **_options: object,
        ) -> dict[int, validation_runner._ProcessRecord]:
            nonlocal calls
            calls += 1
            snapshot = {
                100: validation_runner._ProcessRecord(
                    validation_runner._ProcessIdentity(100, "root"),
                    1,
                    100,
                    100,
                    "T",
                )
            }
            for pid in range(201, 201 + calls):
                snapshot[pid] = validation_runner._ProcessRecord(
                    validation_runner._ProcessIdentity(pid, f"child-{pid}"),
                    100,
                    pid,
                    100,
                    "T",
                )
            return snapshot

        frozen_groups = {100: 0}
        frozen_identities: set[validation_runner._ProcessIdentity] = set()
        with (
            mock.patch.object(
                validation_runner,
                "_process_snapshot",
                side_effect=growing_snapshot,
            ),
            mock.patch.object(
                validation_runner, "_stop_and_confirm_group", return_value=True
            ),
            self.assertRaisesRegex(RuntimeError, "did not reach a frozen fixed point"),
        ):
            validation_runner._freeze_process_tree(
                100, frozen_groups, frozen_identities, time.monotonic() + 0.03
            )

        self.assertGreater(len(frozen_groups), 1)

    @unittest.skipIf(os.name == "nt", "POSIX process cleanup contract")
    def test_process_tree_freeze_absorbs_a_new_group_before_converging(self):
        root = validation_runner._ProcessRecord(
            validation_runner._ProcessIdentity(100, "root"), 1, 100, 100, "T"
        )
        child_running = validation_runner._ProcessRecord(
            validation_runner._ProcessIdentity(201, "child"), 100, 201, 100, "R"
        )
        child_stopped = dataclasses.replace(child_running, state="T")
        snapshots = [
            {100: root},
            {100: root, 201: child_running},
            {100: root, 201: child_stopped},
            {100: root, 201: child_stopped},
            {100: root, 201: child_stopped},
        ]
        frozen_groups = {100: 0}
        frozen_identities: set[validation_runner._ProcessIdentity] = set()

        with (
            mock.patch.object(
                validation_runner,
                "_process_snapshot",
                side_effect=snapshots,
            ),
            mock.patch.object(
                validation_runner, "_stop_and_confirm_group", return_value=True
            ) as stop_group,
            mock.patch.object(validation_runner.time, "sleep"),
        ):
            validation_runner._freeze_process_tree(
                100, frozen_groups, frozen_identities, time.monotonic() + 1
            )

        self.assertEqual(frozen_groups, {100: 0, 201: 1})
        self.assertEqual(
            frozen_identities,
            {
                validation_runner._ProcessIdentity(100, "root"),
                validation_runner._ProcessIdentity(201, "child"),
            },
        )
        self.assertEqual(
            stop_group.call_args.args[:3],
            (100, 201, {child_running.identity}),
        )

    @unittest.skipIf(os.name == "nt", "POSIX process cleanup contract")
    def test_frozen_groups_are_killed_deepest_first_without_resume(self):
        records = {
            pid: validation_runner._ProcessRecord(
                validation_runner._ProcessIdentity(pid, f"start-{pid}"),
                1,
                pid,
                100,
                "T",
            )
            for pid in (100, 200, 300)
        }
        identities = {record.identity for record in records.values()}
        with (
            mock.patch.object(validation_runner.sys, "platform", "darwin"),
            mock.patch.object(
                validation_runner, "_process_snapshot", return_value=records
            ),
            mock.patch.object(validation_runner.os, "killpg") as killpg,
        ):
            errors = validation_runner._kill_frozen_groups(
                100,
                {100: 0, 200: 1, 300: 2},
                identities,
                time.monotonic() + 1,
            )

        self.assertEqual(errors, [])
        self.assertEqual(
            killpg.call_args_list,
            [
                mock.call(300, signal.SIGKILL),
                mock.call(200, signal.SIGKILL),
                mock.call(100, signal.SIGKILL),
            ],
        )

    def test_process_snapshot_rejects_a_malformed_row(self):
        completed = subprocess.CompletedProcess(
            args=["ps"], returncode=0, stdout="not-a-process-row\n", stderr=""
        )
        with (
            mock.patch.object(
                validation_runner.subprocess, "run", return_value=completed
            ),
            self.assertRaisesRegex(RuntimeError, "malformed process inventory row"),
        ):
            validation_runner._process_snapshot(time.monotonic() + 1)

    def test_process_snapshot_ignores_unowned_linux_kernel_rows(self):
        completed = subprocess.CompletedProcess(
            args=["ps"],
            returncode=0,
            stdout="2 0 0 0 S Tue Aug 25 13:05:11 2026\n",
            stderr="",
        )
        with (
            mock.patch.object(
                validation_runner.subprocess, "run", return_value=completed
            ),
            mock.patch.object(validation_runner.sys, "platform", "linux"),
        ):
            snapshot = validation_runner._process_snapshot(time.monotonic() + 1)

        self.assertEqual(snapshot, {})

    def test_linux_process_snapshot_uses_kernel_start_ticks(self):
        completed = subprocess.CompletedProcess(
            args=["ps"],
            returncode=0,
            stdout="200 100 200 100 T Tue Aug 25 13:05:11 2026\n",
            stderr="",
        )
        with (
            mock.patch.object(
                validation_runner.subprocess, "run", return_value=completed
            ) as run,
            mock.patch.object(validation_runner.sys, "platform", "linux"),
            mock.patch.object(
                validation_runner,
                "_read_linux_process_record",
                return_value=validation_runner._ProcessRecord(
                    validation_runner._ProcessIdentity(
                        200, "linux-start-ticks:123"
                    ),
                    100,
                    200,
                    100,
                    "T",
                ),
            ),
            mock.patch.object(validation_runner.os, "getpgid") as getpgid,
            mock.patch.object(validation_runner.os, "getsid") as getsid,
        ):
            snapshot = validation_runner._process_snapshot(time.monotonic() + 1)

        self.assertEqual(snapshot[200].process_group, 200)
        self.assertEqual(snapshot[200].session_id, 100)
        self.assertEqual(snapshot[200].identity.started, "linux-start-ticks:123")
        self.assertIn("sid=", run.call_args.args[0][2])
        getpgid.assert_not_called()
        getsid.assert_not_called()

    def test_linux_process_record_uses_proc_start_ticks(self):
        payload = (
            "200 (worker with spaces) S 100 200 100 0 -1 0 0 0 0 0 0 0 0 "
            "0 20 0 1 0 12345 0\n"
        )
        with mock.patch.object(Path, "read_text", return_value=payload):
            record = validation_runner._read_linux_process_record(200)

        self.assertIsNotNone(record)
        assert record is not None
        self.assertEqual(record.identity.started, "linux-start-ticks:12345")
        self.assertEqual(record.parent_pid, 100)
        self.assertEqual(record.process_group, 200)
        self.assertEqual(record.session_id, 100)

    def test_linux_pidfd_signal_refuses_a_reused_identity(self):
        original = validation_runner._ProcessRecord(
            validation_runner._ProcessIdentity(200, "linux-start-ticks:10"),
            100,
            200,
            100,
            "T",
        )
        replacement = dataclasses.replace(
            original,
            identity=validation_runner._ProcessIdentity(
                200, "linux-start-ticks:11"
            ),
        )
        with (
            mock.patch.object(
                validation_runner.os, "pidfd_open", return_value=9, create=True
            ),
            mock.patch.object(validation_runner.os, "close") as close,
            mock.patch.object(
                validation_runner.signal, "pidfd_send_signal", create=True
            ) as pidfd_signal,
            mock.patch.object(
                validation_runner,
                "_read_linux_process_record",
                return_value=replacement,
            ),
        ):
            errors = validation_runner._signal_linux_identities(
                (original,), signal.SIGKILL, time.monotonic() + 1
            )

        self.assertEqual(errors, [])
        pidfd_signal.assert_not_called()
        close.assert_called_once_with(9)

    def test_darwin_process_snapshot_preserves_identity_across_state_and_parent_change(self):
        first = subprocess.CompletedProcess(
            args=["ps"],
            returncode=0,
            stdout="200 100 200 R Tue Aug 25 13:05:11 2026\n",
            stderr="",
        )
        changed = subprocess.CompletedProcess(
            args=["ps"],
            returncode=0,
            stdout="200 1 200 S Tue Aug 25 13:05:11 2026\n",
            stderr="",
        )
        with (
            mock.patch.object(
                validation_runner.subprocess, "run", side_effect=[first, changed]
            ) as run,
            mock.patch.object(validation_runner.sys, "platform", "darwin"),
            mock.patch.object(validation_runner.os, "getpgid", return_value=200),
            mock.patch.object(validation_runner.os, "getsid", return_value=100),
        ):
            snapshot = validation_runner._process_snapshot(time.monotonic() + 1)

        self.assertEqual(snapshot[200].parent_pid, 1)
        self.assertEqual(snapshot[200].state, "S")
        self.assertEqual(snapshot[200].identity.started, "Tue Aug 25 13:05:11 2026")
        self.assertEqual(run.call_count, 2)
        self.assertNotIn("sid=", run.call_args.args[0][2])

    def test_darwin_process_snapshot_retries_unresolved_group_identity(self):
        completed = subprocess.CompletedProcess(
            args=["ps"],
            returncode=0,
            stdout="200 100 200 S Tue Aug 25 13:05:11 2026\n",
            stderr="",
        )
        groups = iter((201, 200) * 100)
        with (
            mock.patch.object(
                validation_runner.subprocess, "run", return_value=completed
            ),
            mock.patch.object(validation_runner.sys, "platform", "darwin"),
            mock.patch.object(
                validation_runner.os, "getpgid", side_effect=lambda _pid: next(groups)
            ),
            mock.patch.object(validation_runner.os, "getsid", return_value=100),
            self.assertRaisesRegex(
                RuntimeError, "unambiguous snapshot|cleanup exceeded"
            ),
        ):
            validation_runner._process_snapshot(
                time.monotonic() + 0.03, root_pid=100
            )

    def test_darwin_process_snapshot_checks_every_reappeared_ambiguity(self):
        completed = subprocess.CompletedProcess(
            args=["ps"],
            returncode=0,
            stdout=(
                "100 1 100 T Tue Aug 25 13:05:10 2026\n"
                "200 1 200 T Tue Aug 25 13:05:11 2026\n"
                "201 100 201 T Tue Aug 25 13:05:12 2026\n"
            ),
            stderr="",
        )

        def observe_group(pid: int) -> int:
            if pid == 100:
                return 100
            raise ProcessLookupError

        with (
            mock.patch.object(
                validation_runner.subprocess, "run", return_value=completed
            ),
            mock.patch.object(validation_runner.sys, "platform", "darwin"),
            mock.patch.object(
                validation_runner.os, "getpgid", side_effect=observe_group
            ),
            mock.patch.object(validation_runner.os, "getsid", return_value=100),
            self.assertRaisesRegex(
                RuntimeError, "unambiguous snapshot|cleanup exceeded"
            ),
        ):
            validation_runner._process_snapshot(
                time.monotonic() + 0.03, root_pid=100
            )

    def test_process_snapshot_rejects_an_oversized_inventory(self):
        completed = subprocess.CompletedProcess(
            args=["ps"],
            returncode=0,
            stdout="x" * (validation_runner.MAX_PROCESS_CENSUS_BYTES + 1),
            stderr="",
        )
        with (
            mock.patch.object(
                validation_runner.subprocess, "run", return_value=completed
            ),
            mock.patch.object(validation_runner.sys, "platform", "linux"),
            self.assertRaisesRegex(RuntimeError, "byte limit"),
        ):
            validation_runner._process_snapshot(time.monotonic() + 1)

    def test_post_kill_verification_waits_for_recorded_identities_to_disappear(self):
        identity = validation_runner._ProcessIdentity(200, "start")
        running = validation_runner._RawProcessRecord(100, 200, None, "R", "start")
        sleeping = validation_runner._RawProcessRecord(1, 200, None, "S", "start")
        with (
            mock.patch.object(
                validation_runner,
                "_read_process_rows",
                side_effect=[{200: running}, {200: sleeping}, {}, {}],
            ) as census,
            mock.patch.object(validation_runner.sys, "platform", "darwin"),
            mock.patch.object(validation_runner.time, "sleep"),
        ):
            validation_runner._verify_processes_gone(
                {identity}, time.monotonic() + 1
            )

        self.assertEqual(census.call_count, 4)

    def test_post_kill_verification_accepts_a_zombie_as_non_executable(self):
        identity = validation_runner._ProcessIdentity(200, "start")
        zombie = validation_runner._RawProcessRecord(1, 200, None, "Z", "start")
        with mock.patch.object(
            validation_runner,
            "_read_process_rows",
            side_effect=[{200: zombie}, {200: zombie}],
        ) as census, mock.patch.object(validation_runner.sys, "platform", "darwin"):
            validation_runner._verify_processes_gone(
                {identity}, time.monotonic() + 1
            )

        self.assertEqual(census.call_count, 2)

    @unittest.skipIf(os.name == "nt", "POSIX process cleanup contract")
    def test_timeout_termination_invokes_recorded_identity_verification(self):
        process = mock.Mock()
        process.pid = 100
        process.poll.return_value = 0
        identity = validation_runner._ProcessIdentity(200, "start")

        def freeze(
            _root_pid: int,
            _groups: dict[int, int],
            identities: set[validation_runner._ProcessIdentity],
            _deadline: float,
            *,
            attempted_groups: dict[int, int],
        ) -> None:
            self.assertEqual(attempted_groups, {})
            identities.add(identity)

        with (
            mock.patch.object(validation_runner.os, "killpg"),
            mock.patch.object(validation_runner, "_freeze_process_tree", side_effect=freeze),
            mock.patch.object(validation_runner, "_kill_frozen_groups", return_value=[]),
            mock.patch.object(validation_runner, "_reap_owned_process", return_value=None),
            mock.patch.object(validation_runner, "_verify_processes_gone") as verify,
        ):
            validation_runner._terminate_process_tree(process, time.monotonic() + 1)

        self.assertEqual(verify.call_args.args[0], {identity})

    @unittest.skipIf(os.name == "nt", "POSIX process cleanup contract")
    def test_kill_stage_exception_still_kills_and_reaps_owned_shell(self):
        process = mock.Mock()
        process.pid = 100
        process.poll.return_value = None
        process.wait.return_value = 0

        with (
            mock.patch.object(validation_runner.os, "killpg"),
            mock.patch.object(validation_runner, "_freeze_process_tree"),
            mock.patch.object(
                validation_runner,
                "_kill_frozen_groups",
                side_effect=RuntimeError("injected final census failure"),
            ),
            mock.patch.object(validation_runner, "_verify_processes_gone") as verify,
            self.assertRaisesRegex(RuntimeError, "kill stage"),
        ):
            validation_runner._terminate_process_tree(
                process, time.monotonic() + 1
            )

        process.kill.assert_called_once_with()
        process.wait.assert_called_once()
        verify.assert_not_called()

    @unittest.skipIf(os.name == "nt", "POSIX process cleanup contract")
    def test_linux_final_census_failure_still_kills_and_reaps_owned_shell(self):
        process = mock.Mock()
        process.pid = 100
        process.poll.return_value = None
        process.wait.return_value = 0
        child = validation_runner._ProcessRecord(
            validation_runner._ProcessIdentity(200, "linux-start-ticks:10"),
            100,
            200,
            100,
            "T",
        )

        def freeze(
            _root_pid: int,
            groups: dict[int, int],
            identities: set[validation_runner._ProcessIdentity],
            _deadline: float,
            *,
            attempted_groups: dict[int, int],
        ) -> None:
            self.assertEqual(attempted_groups, {})
            groups[200] = 1
            identities.add(child.identity)

        with (
            mock.patch.object(validation_runner.sys, "platform", "linux"),
            mock.patch.object(validation_runner.os, "killpg"),
            mock.patch.object(
                validation_runner, "_freeze_process_tree", side_effect=freeze
            ),
            mock.patch.object(
                validation_runner,
                "_process_snapshot",
                side_effect=[
                    {200: child},
                    RuntimeError("injected second Linux census failure"),
                    {},
                ],
            ),
            mock.patch.object(
                validation_runner.os, "pidfd_open", return_value=9, create=True
            ),
            mock.patch.object(validation_runner.os, "close") as close,
            mock.patch.object(
                validation_runner.signal, "pidfd_send_signal", create=True
            ) as pidfd_signal,
            self.assertRaisesRegex(RuntimeError, "second Linux census failure"),
        ):
            validation_runner._terminate_process_tree(
                process, time.monotonic() + 1
            )

        pidfd_signal.assert_not_called()
        close.assert_called_once_with(9)
        process.kill.assert_called_once_with()
        process.wait.assert_called_once()

    @unittest.skipIf(os.name == "nt", "POSIX process cleanup contract")
    def test_post_stop_census_failure_kills_attempted_owned_group(self):
        process = mock.Mock()
        process.pid = 100
        process.poll.return_value = None
        process.wait.return_value = 0
        root = validation_runner._ProcessRecord(
            validation_runner._ProcessIdentity(100, "root"), 1, 100, 100, "T"
        )
        child = validation_runner._ProcessRecord(
            validation_runner._ProcessIdentity(200, "child"),
            100,
            200,
            100,
            "T",
        )

        with (
            mock.patch.object(validation_runner.sys, "platform", "darwin"),
            mock.patch.object(validation_runner.os, "killpg") as killpg,
            mock.patch.object(
                validation_runner,
                "_process_snapshot",
                side_effect=[
                    {100: root, 200: dataclasses.replace(child, state="R")},
                    RuntimeError("injected post-stop census failure"),
                    {100: root, 200: child},
                    {100: root, 200: child},
                ],
            ),
            self.assertRaisesRegex(RuntimeError, "could not prove"),
        ):
            validation_runner._terminate_process_tree(
                process, time.monotonic() + 1
            )

        self.assertIn(mock.call(200, signal.SIGKILL), killpg.call_args_list)
        process.kill.assert_called_once_with()
        process.wait.assert_called_once()

    def test_reused_process_group_is_not_killed(self):
        original = validation_runner._ProcessIdentity(200, "start-old")
        replacement = validation_runner._ProcessRecord(
            validation_runner._ProcessIdentity(201, "start-new"),
            1,
            200,
            200,
            "T",
        )
        with (
            mock.patch.object(validation_runner.sys, "platform", "darwin"),
            mock.patch.object(
                validation_runner,
                "_process_snapshot",
                return_value={201: replacement},
            ),
            mock.patch.object(validation_runner.os, "killpg") as killpg,
        ):
            errors = validation_runner._kill_frozen_groups(
                100, {200: 1}, {original}, time.monotonic() + 1
            )

        self.assertRegex(errors[0], "acquired unowned identities")
        killpg.assert_not_called()

    @unittest.skipIf(os.name == "nt", "POSIX process cleanup contract")
    def test_reused_group_after_stop_is_never_committed_or_killed(self):
        root = validation_runner._ProcessRecord(
            validation_runner._ProcessIdentity(100, "root"), 1, 100, 100, "T"
        )
        child = validation_runner._ProcessRecord(
            validation_runner._ProcessIdentity(200, "child-old"),
            100,
            200,
            100,
            "R",
        )
        replacement = validation_runner._ProcessRecord(
            validation_runner._ProcessIdentity(201, "child-new"),
            1,
            200,
            200,
            "T",
        )
        snapshots = [
            {100: root, 200: child},
            {100: root, 201: replacement},
        ]
        frozen_groups = {100: 0}
        attempted_groups: dict[int, int] = {}
        frozen_identities: set[validation_runner._ProcessIdentity] = set()
        with (
            mock.patch.object(validation_runner.sys, "platform", "darwin"),
            mock.patch.object(
                validation_runner, "_process_snapshot", side_effect=snapshots
            ),
            mock.patch.object(validation_runner.os, "killpg") as killpg,
            mock.patch.object(validation_runner.time, "sleep"),
            self.assertRaisesRegex(RuntimeError, "changed ownership after stop"),
        ):
            validation_runner._freeze_process_tree(
                100,
                frozen_groups,
                frozen_identities,
                time.monotonic() + 1,
                attempted_groups=attempted_groups,
            )

        self.assertNotIn(200, frozen_groups)
        self.assertEqual(attempted_groups, {200: 1})
        self.assertIn(mock.call(200, signal.SIGSTOP), killpg.call_args_list)
        self.assertNotIn(mock.call(200, signal.SIGKILL), killpg.call_args_list)

    def test_windows_tree_kill_nonzero_is_always_fatal(self):
        process = mock.Mock()
        process.pid = 4242
        process.poll.return_value = 0
        process.wait.return_value = 0
        completed = subprocess.CompletedProcess(
            args=["taskkill"], returncode=5, stdout="", stderr=""
        )

        with (
            mock.patch.object(validation_runner.os, "name", "nt"),
            mock.patch.object(
                validation_runner.subprocess, "run", return_value=completed
            ),
            self.assertRaisesRegex(RuntimeError, "taskkill refused.*5"),
        ):
            validation_runner._terminate_process_tree(process)

        process.wait.assert_not_called()

    def test_windows_tree_kill_timeout_is_fatal_and_reaps_shell(self):
        process = mock.Mock()
        process.pid = 4242
        process.poll.return_value = None
        process.wait.return_value = 0
        timeout = subprocess.TimeoutExpired(cmd="taskkill", timeout=10)

        with (
            mock.patch.object(validation_runner.os, "name", "nt"),
            mock.patch.object(
                validation_runner.subprocess, "run", side_effect=timeout
            ),
            self.assertRaisesRegex(RuntimeError, "timed out terminating"),
        ):
            validation_runner._terminate_process_tree(process)

        process.kill.assert_called_once_with()
        process.wait.assert_called_once()

    def test_windows_tree_kill_launch_error_is_fatal_and_reaps_shell(self):
        process = mock.Mock()
        process.pid = 4242
        process.poll.return_value = None
        process.wait.return_value = 0

        with (
            mock.patch.object(validation_runner.os, "name", "nt"),
            mock.patch.object(
                validation_runner.subprocess,
                "run",
                side_effect=FileNotFoundError("taskkill missing"),
            ),
            self.assertRaisesRegex(RuntimeError, "could not launch Windows"),
        ):
            validation_runner._terminate_process_tree(process)

        process.kill.assert_called_once_with()
        process.wait.assert_called_once()

    def test_windows_tree_kill_success_waits_within_cleanup_deadline(self):
        process = mock.Mock()
        process.pid = 4242
        process.poll.return_value = None
        process.wait.return_value = 0
        completed = subprocess.CompletedProcess(
            args=["taskkill"], returncode=0, stdout="", stderr=""
        )

        with (
            mock.patch.object(validation_runner.os, "name", "nt"),
            mock.patch.object(
                validation_runner.subprocess, "run", return_value=completed
            ),
        ):
            validation_runner._terminate_process_tree(process)

        process.kill.assert_not_called()
        process.wait.assert_called_once()

    def test_reports_preserve_point_status_and_parse_as_junit(self):
        catalog = self.load()
        selection = select_named(catalog, ("core",))
        results = [
            CheckResult(
                id="baseline",
                title="Baseline",
                status="passed",
                seconds=0.25,
                returncode=0,
                message="exit 0",
                log_path="logs/baseline.log",
                output="ok\n",
            )
        ]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            json_path, xml_path = write_reports(
                catalog, selection, "commit", results, root / "artifacts", root
            )
            report = json.loads(json_path.read_text(encoding="utf-8"))
            suite = ET.parse(xml_path).getroot()
        self.assertEqual(report["points"][0]["status"], "passed")
        self.assertEqual(report["checks"][0]["status"], "passed")
        self.assertEqual(suite.attrib["tests"], "1")
        self.assertEqual(suite.attrib["failures"], "0")


ROUTING_CATALOG = """
version = 1

[settings]
profiles = ["commit", "ci", "full"]

[[checks]]
id = "rust-gate"
title = "Workspace suite"
command = "make rust-check"
profiles = ["commit", "ci", "full"]

[[checks]]
id = "cluster-auth"
title = "Three-voter replicated state"
command = "make cluster-check"
profiles = ["ci", "full"]
timeout_seconds = {cluster_check_timeout}

[[points]]
id = "cluster.auth"
title = "Replicated durable state"
contract = "{cluster_auth_contract}"
paths = [{cluster_auth_paths}]
checks = ["rust-gate", "cluster-auth"]

[[points]]
id = "cluster.membership"
title = "Cluster membership lifecycle"
contract = "Removal preserves quorum."
paths = ["crates/plurx-core/src/cluster/membership.rs"]
checks = ["rust-gate", "cluster-auth"]

[[points]]
id = "cluster.page-reads"
title = "Clustered page reads"
contract = "Pages read one authority-consistent query."
paths = ["crates/plurxd/src/http/browse.rs"]
checks = ["rust-gate", "cluster-auth"]

[[points]]
id = "web.experience"
title = "Browser experience"
contract = "{web_contract}"
paths = [{web_paths}]
checks = ["rust-gate"]
"""


def routing_catalog(
    *,
    cluster_auth_paths: tuple[str, ...] = (
        "crates/plurx-core/src/store/hiqlite.rs",
        "crates/plurxd/src/http/cluster.rs",
    ),
    cluster_auth_contract: str = "Three voters agree on the replicated Store.",
    cluster_check_timeout: int = 3600,
    web_paths: tuple[str, ...] = ("crates/plurxd/src/web/app.js",),
    web_contract: str = "The browser renders every library.",
) -> str:
    """Render a small catalog whose cluster selectors can be edited one at a time."""

    def literal(paths: tuple[str, ...]) -> str:
        return ", ".join(f'"{path}"' for path in paths)

    return ROUTING_CATALOG.format(
        cluster_auth_paths=literal(cluster_auth_paths),
        cluster_auth_contract=cluster_auth_contract,
        cluster_check_timeout=cluster_check_timeout,
        web_paths=literal(web_paths),
        web_contract=web_contract,
    )


class CatalogRoutingScopeCase(unittest.TestCase):
    """A `points.toml`-only routing edit runs the Store lane on its content.

    Touching any `CI_ROUTING_PATHS` entry used to force `cluster_auth`, and that
    lane is `replicated Store contracts (legacy)` — 26-29 minutes, and on
    2026-09-02 a queue that reached 234-minute waits with 27 jobs cancelled
    before they started. Three of the four PRs that took a full `ci` run in a
    four-day sample pulled the lane in by that rule rather than by touching
    cluster code; #830 paid 27 minutes for editing `validation/points.toml`.
    The rule's reason survives intact — a catalog edit that could hide the
    cluster lanes still runs them — and only an edit that provably could not,
    prose or a point the lane never reads, goes free.
    """

    def git(self, root: Path, *arguments: str) -> str:
        return subprocess.run(
            ["git", *arguments],
            cwd=root,
            check=True,
            text=True,
            stdout=subprocess.PIPE,
        ).stdout.strip()

    @contextlib.contextmanager
    def diff_against_base(
        self, base_catalog: str | None, head_files: dict[str, str]
    ):
        """Yield (root, base sha) for a branch that writes `head_files`.

        The fixture is a real repository because the narrowing reads the base
        revision's blob, which no path list can stand in for. A `base_catalog`
        of `None` seeds a base revision with no catalog at all.
        """

        seed = {
            ".github/workflows/ci.yml": "name: ci\n",
            "crates/plurx-core/src/store/hiqlite.rs": "pub fn store() {}\n",
            "crates/plurxd/src/web/app.js": "export const app = 1;\n",
        }
        if base_catalog is not None:
            seed["validation/points.toml"] = base_catalog
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for relative, content in seed.items():
                path = root / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(content, encoding="utf-8")
            self.git(root, "init", "-q")
            self.git(root, "checkout", "-qb", "main")
            self.git(root, "config", "user.name", "Scope Test")
            self.git(root, "config", "user.email", "scope@example.invalid")
            self.git(root, "add", "-A")
            self.git(root, "commit", "-qm", "seed")
            base = self.git(root, "rev-parse", "HEAD")

            self.git(root, "checkout", "-qb", "feature")
            for relative, content in head_files.items():
                path = root / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(content, encoding="utf-8")
            self.git(root, "add", "-A")
            self.git(root, "commit", "-qm", "branch work")
            yield root, base

    def scope(self, root: Path, base: str) -> tuple[dict[str, bool], str]:
        """Run the production entry point against the fixture repository.

        `resolve_scope` reads `REPO_ROOT` and the default catalog, so both are
        redirected at the fixture. `load_catalog` keeps its real behavior when
        it is handed an explicit path, which is how the base blob is parsed.
        """

        real_load_catalog = load_catalog
        catalog_path = root / "validation/points.toml"
        stderr = io.StringIO()
        with mock.patch.dict(
            ci_scope.__dict__,
            {
                "REPO_ROOT": root,
                "load_catalog": lambda path=catalog_path: real_load_catalog(path),
            },
        ):
            with contextlib.redirect_stderr(stderr):
                scope = resolve_scope("pull_request", base)
        return scope, stderr.getvalue()

    def test_narrowed_paths_on_a_cluster_point_still_run_the_store_lane(self):
        # Dropping a path from `cluster.auth` is exactly the suppression the
        # routing rule exists to catch: after this edit a store change no longer
        # selects the lane, so the edit itself has to run it.
        narrowed = routing_catalog(
            cluster_auth_paths=("crates/plurxd/src/http/cluster.rs",)
        )
        with self.diff_against_base(
            routing_catalog(), {"validation/points.toml": narrowed}
        ) as (root, base):
            scope, stderr = self.scope(root, base)

        self.assertTrue(scope["cluster_auth"])
        self.assertTrue(scope["rust"])
        self.assertEqual(stderr, "")

    def test_prose_on_a_cluster_point_does_not_run_the_store_lane(self):
        # A `contract` string is documentation. It cannot select or suppress a
        # single check, and it is the case the narrowing exists to make cheap.
        with self.diff_against_base(
            routing_catalog(),
            {
                "validation/points.toml": routing_catalog(
                    cluster_auth_contract="Three voters agree, and say so twice."
                )
            },
        ) as (root, base):
            scope, stderr = self.scope(root, base)

        self.assertFalse(scope["cluster_auth"])
        # The Rust lane stays forced for every routing path, the catalog included.
        self.assertTrue(scope["rust"])
        self.assertEqual(stderr, "")

    def test_an_unrelated_point_does_not_run_the_store_lane(self):
        # `web.experience` is not one of the three points `cluster_auth` reads,
        # so rewriting it whole — paths and prose — hides no cluster evidence.
        with self.diff_against_base(
            routing_catalog(),
            {
                "validation/points.toml": routing_catalog(
                    web_paths=("crates/plurxd/src/web/**",),
                    web_contract="The browser renders every library and its art.",
                )
            },
        ) as (root, base):
            scope, stderr = self.scope(root, base)

        self.assertFalse(scope["cluster_auth"])
        self.assertTrue(scope["rust"])
        self.assertEqual(stderr, "")

    def test_a_field_of_the_cluster_auth_check_runs_the_store_lane(self):
        # The check record is the lane's own definition. A timeout, command,
        # profile or platform edit decides whether that evidence runs at all.
        with self.diff_against_base(
            routing_catalog(),
            {"validation/points.toml": routing_catalog(cluster_check_timeout=600)},
        ) as (root, base):
            scope, _ = self.scope(root, base)

        self.assertTrue(scope["cluster_auth"])

    def test_any_other_routing_path_alongside_the_catalog_runs_the_store_lane(self):
        # The narrowing is only consulted when the catalog is the sole routing
        # path in the diff. A workflow edit changes the job graph, which is not
        # reducible to a catalog comparison, so it forces the lane even beside a
        # catalog edit that on its own would not have.
        with self.diff_against_base(
            routing_catalog(),
            {
                "validation/points.toml": routing_catalog(
                    cluster_auth_contract="Three voters agree, restated."
                ),
                ".github/workflows/ci.yml": "name: ci\non: [push]\n",
            },
        ) as (root, base):
            scope, _ = self.scope(root, base)

        self.assertTrue(scope["cluster_auth"])
        self.assertFalse(
            catalog_is_the_only_routing_edit(
                (".github/workflows/ci.yml", "validation/points.toml")
            )
        )
        # Non-routing paths beside the catalog leave the narrowing available.
        self.assertTrue(
            catalog_is_the_only_routing_edit(
                ("docs/VALIDATION.md", "validation/points.toml")
            )
        )
        # `scope_for_paths` holds that line itself. Told outright that the
        # catalog edit hides nothing, it still keeps the lane for the workflow
        # path beside it, so no caller can release a routing path this rule
        # cannot reason about.
        self.assertTrue(
            scope_for_paths(
                load_catalog(ROOT / "validation/points.toml"),
                (".github/workflows/ci.yml", "validation/points.toml"),
                catalog_edit_hides_cluster_lanes=False,
            )["cluster_auth"]
        )

    def test_an_unreadable_or_unparseable_base_runs_the_store_lane_and_says_so(self):
        # Every uncertainty resolves to running the lane. Failing closed here
        # would silently drop cluster evidence, which costs far more than the
        # half hour the narrowing saves, so the reason is printed too.
        with self.diff_against_base(
            None, {"validation/points.toml": routing_catalog()}
        ) as (root, base):
            unreadable, stderr = self.scope(root, base)

        self.assertTrue(unreadable["cluster_auth"])
        self.assertIn("cannot compare validation/points.toml", stderr)
        self.assertIn("enabling the replicated Store lane", stderr)
        self.assertEqual(len(stderr.strip().splitlines()), 1)

        with self.diff_against_base(
            "version = 1\nthis is not toml\n",
            {"validation/points.toml": routing_catalog()},
        ) as (root, base):
            unparseable, stderr = self.scope(root, base)

        self.assertTrue(unparseable["cluster_auth"])
        self.assertIn("cannot compare validation/points.toml", stderr)

    def test_a_real_cluster_path_runs_the_store_lane_without_the_catalog(self):
        # The ordinary points selection is untouched: a store change still
        # selects `cluster.auth` and therefore the lane, with no catalog edit
        # and no base comparison involved.
        with self.diff_against_base(
            routing_catalog(),
            {
                "crates/plurx-core/src/store/hiqlite.rs": (
                    "pub fn store() { let _ = 1; }\n"
                )
            },
        ) as (root, base):
            scope, stderr = self.scope(root, base)

        self.assertTrue(scope["cluster_auth"])
        self.assertEqual(stderr, "")

    def test_the_default_answer_forces_the_lane_for_a_path_only_caller(self):
        # `scope_for_paths` stays pure over paths. A caller that knows nothing
        # about the base revision gets the conservative answer, which is why
        # every existing path-only assertion about `points.toml` still holds.
        catalog = load_catalog(ROOT / "validation/points.toml")
        self.assertTrue(
            scope_for_paths(catalog, ("validation/points.toml",))["cluster_auth"]
        )
        self.assertFalse(
            scope_for_paths(
                catalog,
                ("validation/points.toml",),
                catalog_edit_hides_cluster_lanes=False,
            )["cluster_auth"]
        )

    def test_the_compared_selectors_are_the_ones_the_lane_actually_reads(self):
        # A rename in the catalog would otherwise make the narrowing stale in
        # the dangerous direction: unknown ids compare equal forever, so the
        # comparison would stop noticing real suppression. Pin both constants
        # against the shipped catalog.
        catalog = load_catalog(ROOT / "validation/points.toml")
        self.assertEqual(
            set(ci_scope.CLUSTER_LANE_POINTS),
            {"cluster.auth", "cluster.membership", "cluster.page-reads"},
        )
        self.assertTrue(
            set(ci_scope.CLUSTER_LANE_POINTS) <= set(catalog.point_map)
        )
        self.assertIn(ci_scope.CLUSTER_LANE_CHECK, catalog.check_map)
        self.assertIn(ci_scope.CATALOG_ROUTING_PATH, CI_ROUTING_PATHS)
        for point_id in ci_scope.CLUSTER_LANE_POINTS:
            with self.subTest(point_id=point_id):
                self.assertIn(
                    ci_scope.CLUSTER_LANE_CHECK, catalog.point_map[point_id].checks
                )


if __name__ == "__main__":
    unittest.main()
