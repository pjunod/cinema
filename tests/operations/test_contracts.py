from __future__ import annotations

import json
import os
from pathlib import Path
import re
import runpy
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]


def read(path: str) -> str:
    return (ROOT / path).read_text(encoding="utf-8")


def workflow_job_blocks(path: str) -> dict[str, str]:
    jobs = read(path).split("\njobs:\n", 1)[1]
    starts = list(re.finditer(r"(?m)^  ([a-zA-Z0-9_-]+):\n", jobs))
    return {
        match.group(1): jobs[match.start() : starts[index + 1].start()]
        if index + 1 < len(starts)
        else jobs[match.start() :]
        for index, match in enumerate(starts)
    }


def workflow_step_blocks(job: str) -> dict[str, str]:
    starts = list(re.finditer(r"(?m)^      - name: (.+)\n", job))
    return {
        match.group(1): job[match.start() : starts[index + 1].start()]
        if index + 1 < len(starts)
        else job[match.start() :]
        for index, match in enumerate(starts)
    }


def workflow_step_scalar(step: str, key: str) -> str:
    values = re.findall(rf"(?m)^        {re.escape(key)}: ([^\n]+)$", step)
    if len(values) != 1:
        raise AssertionError(f"expected one scalar {key!r}, found {len(values)}")
    return values[0]


class OperationsContractCase(unittest.TestCase):
    def test_browser_validation_never_claims_host_audio_or_media_controls(self):
        for path in ("scripts/playback-lab", "scripts/ui-baseline"):
            with self.subTest(path=path):
                script = read(path)
                self.assertIn('"--mute-audio"', script)
                self.assertIn("HardwareMediaKeyHandling", script)

    def test_ui_baseline_starts_poll_observation_after_route_settles(self):
        script = read("scripts/ui-baseline")

        self.assertIn(
            'if name in {"home", "activity", "analysis", "settings"}:', script
        )
        self.assertIn('[data-phase="settled"]', script)
        self.assertIn('wait_until="domcontentloaded"', script)
        self.assertIn("activity_poll_paused = pause_activity_polling", script)
        self.assertIn('page.wait_for_load_state("networkidle"', script)
        self.assertIn("resume_activity_polling(page)", script)
        self.assertIn('api_calls.count("GET /api/v1/scan/status") < 3', script)
        self.assertIn('api_calls.count("GET /api/v1/activity") < 2', script)
        self.assertIn('if name == "analysis":', script)
        self.assertIn("if (PAGE_TIMER) clearInterval(PAGE_TIMER);", script)
        self.assertIn("if (ACT_TIMER) clearInterval(ACT_TIMER);", script)
        player_open = script.index(
            'if route.get("player"):', script.index("def capture_route")
        )
        self.assertLess(
            script.index("pause_activity_polling(page, name, args.timeout)", player_open),
            script.index("open_player(page, args.timeout)", player_open),
        )
        self.assertLess(
            script.index('[data-phase="settled"]'),
            script.index("resume_activity_polling(page)", script.index("def capture_route")),
        )
        self.assertLess(
            script.index("resume_activity_polling(page)", script.index("def capture_route")),
            script.index("page.wait_for_timeout(settle_ms)"),
        )

    def test_ui_baseline_pins_vod_index_status_before_seeding_libraries(self):
        script = read("scripts/ui-baseline")
        seed = script.index("    def seed(self):")
        pinned = script.index('{"vod_index_mins": 0}', seed)
        libraries = script.index("for name, kind, share in LIBRARIES:", seed)

        self.assertLess(pinned, libraries)

    def test_ui_baseline_releases_players_and_narrowly_retries_root_attachment(self):
        script = read("scripts/ui-baseline")
        contract = runpy.run_path(
            str(ROOT / "scripts/ui-baseline"), run_name="ui_baseline_contract"
        )
        should_retry = contract["should_retry_capture"]
        classify_root = contract["root_attach_failure"]
        fail = contract["Fail"]
        root_attach = contract["RetryableRootAttach"]

        self.assertIn("releaseSession(PLAYER.sessionId);", script)
        self.assertIn(
            'call == "GET /api/v1/files/<id>/direct"',
            script,
        )
        cleanup = script.index(
            "releaseSession(PLAYER.sessionId);", script.index("def capture_route")
        )
        self.assertNotIn("closePlayer();", script[cleanup : cleanup + 500])
        self.assertIn("class RetryableRootAttach(Fail):", script)
        self.assertIn("except PlaywrightTimeoutError as error:", script)
        self.assertIn("attempts = 2", script)
        self.assertIn(
            "if should_retry_capture(route, e, attempt, attempts):",
            script,
        )
        self.assertIn('print(f"RETRY   {key}: {e}"', script)
        self.assertIn('print(f"FAIL    {key}: {e}"', script)
        semantic_root = classify_root("library", "#main", ["pageerror: boot failed"])
        transient_root = classify_root("library", "#main", [])
        self.assertIs(type(semantic_root), fail)
        self.assertIs(type(transient_root), root_attach)
        self.assertFalse(should_retry({}, semantic_root, 0, 2))
        self.assertTrue(should_retry({}, root_attach("root"), 0, 2))
        self.assertFalse(should_retry({}, root_attach("root"), 1, 2))
        self.assertFalse(should_retry({}, fail("semantic"), 0, 2))
        self.assertTrue(should_retry({"player": True}, fail("legacy player"), 0, 2))
        self.assertFalse(should_retry({"player": True}, fail("legacy player"), 1, 2))
        self.assertLess(
            cleanup,
            script.index("page.close()", script.index("def capture_route")),
        )

    def test_rust_test_artifacts_omit_replicated_debug_information(self):
        cargo = read("Cargo.toml")
        profile = cargo.split("[profile.test]", 1)[1].split("[", 1)[0]

        self.assertRegex(profile, r"(?m)^debug = 0$")

        spike = read("spikes/hiqlite-m0/Cargo.toml")
        for name in ("dev", "test"):
            with self.subTest(workspace="hiqlite-m0", profile=name):
                profile = spike.split(f"[profile.{name}]", 1)[1].split("[", 1)[0]
                self.assertRegex(profile, r"(?m)^debug = 0$")

    def test_swarm_runtime_keeps_network_quota_and_role_boundaries_explicit(self):
        config = json.loads(read("swarm/config.json"))

        self.assertIs(config["runtime"]["worker_network_access"], True)
        self.assertIs(config["queue"]["prefer_expiring_quota"], True)
        self.assertEqual(config["queue"]["diagnosis_role"], "troubleshooter")
        self.assertEqual(config["queue"]["diagnosis_fix_role"], "builder")
        self.assertEqual(
            config["workers"]["troubleshooter"]["role"], "troubleshooter"
        )
        troubleshooter = read("swarm/troubleshooter.txt")
        self.assertIn("Stay read-only", troubleshooter)
        self.assertIn("Root cause and confidence", troubleshooter)
        self.assertIn("diagnosis-fix", troubleshooter)
        for name, worker in config["workers"].items():
            with self.subTest(worker=name):
                self.assertNotIn("fallback_roles", worker)

    def test_compose_keeps_identity_data_and_host_ports_explicit(self):
        compose = read("deploy/docker-compose.yml")
        self.assertRegex(compose, r"(?m)^name: plurx$")
        self.assertIn('${PLURX_DATA:-/srv/plurx}:/var/lib/plurx', compose)
        self.assertIn('${PLURX_HTTP_PORT:-32400}:32400', compose)
        self.assertIn('${PLURX_GDM_PORT:-32414}:32414/udp', compose)
        self.assertIn('user: "${PUID:-1000}:${PGID:-1000}"', compose)
        self.assertIn('PLURX_BUILD_REF: ${PLURX_BUILD_REF:-}', compose)
        self.assertIn(
            "stop_grace_period: ${PLURX_STOP_GRACE_PERIOD:-65m}", compose
        )
        self.assertIn(
            'PLURX_NODE_HOSTNAME: "${PLURX_NODE_HOSTNAME:-${HOSTNAME:-}}"',
            compose,
        )

    def test_discovery_uses_host_network_without_stealing_it_from_the_server(self):
        compose = read("deploy/docker-compose.yml")
        server, discovery = compose.split("  plurx-discovery:", 1)
        self.assertNotRegex(server, r"(?m)^    network_mode: host$")
        self.assertIn("network_mode: host", discovery)
        configured_origin = '"http://127.0.0.1:${PLURX_HTTP_PORT:-32400}"'
        configured_bind = '"127.0.0.1:${PLURX_HTTP_PORT:-32400}"'
        self.assertIn(f"PLURX_DISCOVERY_SERVER_URL: {configured_origin}", discovery)
        self.assertIn(f"PLURX_BIND: {configured_bind}", discovery)

    def test_docker_up_preserves_override_discovery_and_stamps_the_build(self):
        result = subprocess.run(
            ["make", "-n", "docker-up"],
            cwd=ROOT,
            check=True,
            text=True,
            stdout=subprocess.PIPE,
        )
        command = result.stdout
        self.assertIn("cd deploy && PLURX_BUILD_REF=", command)
        self.assertIn("PLURX_NODE_HOSTNAME=", command)
        self.assertIn("docker compose up -d --build", command)
        self.assertNotIn("-f deploy/docker-compose.yml", command)

        makefile = read("Makefile")
        self.assertIn("HOST_SHORTNAME := $(shell hostname -s", makefile)
        self.assertIn('PLURX_NODE_HOSTNAME="$(HOST_SHORTNAME)"', makefile)

        dockerfile = read("Dockerfile")
        compose = read("deploy/docker-compose.yml")
        self.assertIn('ARG PLURX_BUILD_REF=""', dockerfile)
        self.assertIn("ENV PLURX_BUILD_REF=${PLURX_BUILD_REF}", dockerfile)
        self.assertIn('ARG PLURX_BUILD_SHA=""', dockerfile)
        self.assertIn("ENV PLURX_BUILD_SHA=${PLURX_BUILD_SHA}", dockerfile)
        self.assertIn("PLURX_BUILD_REF: ${PLURX_BUILD_REF:-}", compose)

    def test_docker_build_frees_each_ffmpeg_download_before_the_next(self):
        dockerfile = read("Dockerfile")
        distro_install = dockerfile.index("intel-media-va-driver-non-free")
        first_clean = dockerfile.index("apt-get clean", distro_install)
        jellyfin_install = dockerfile.index("apt-get install -y --no-install-recommends jellyfin-ffmpeg7")
        second_clean = dockerfile.index("apt-get clean", jellyfin_install)

        self.assertLess(distro_install, first_clean)
        self.assertLess(first_clean, jellyfin_install)
        self.assertLess(jellyfin_install, second_clean)
        self.assertEqual(dockerfile.count("&& apt-get clean"), 2)

    def test_docker_build_requires_the_profile5_renderer_used_at_runtime(self):
        dockerfile = read("Dockerfile")
        self.assertIn("-h filter=tonemapx", dockerfile)
        self.assertIn(
            "grep -q '^[[:space:]]*apply_dovi[[:space:]]'",
            dockerfile,
        )
        self.assertNotIn("-h filter=libplacebo", dockerfile)
        self.assertNotIn("apply_dolbyvision", dockerfile)

    def test_docker_build_pins_and_verifies_disk_conversion_tools(self):
        dockerfile = read("Dockerfile")
        self.assertIn("ARG DOVI_TOOL_VERSION=2.3.3", dockerfile)
        self.assertIn("ARG MKVTOOLNIX_VERSION=74.0.0-1", dockerfile)
        self.assertIn('"mkvtoolnix=${MKVTOOLNIX_VERSION}"', dockerfile)
        self.assertIn(
            "dovi_sha=5dae82cb2becd3b9fd726127f936a8d32635e60746d16238fdfded12aa05988c",
            dockerfile,
        )
        self.assertIn(
            "dovi_sha=daf538c275f4e702219ce8eb61db28382193ac9d0126e1ef4185a88303af4485",
            dockerfile,
        )
        self.assertIn('sha256sum -c -', dockerfile)
        self.assertIn('dovi_tool --version | grep -F "${DOVI_TOOL_VERSION}"', dockerfile)
        self.assertIn('mkvmerge --version | grep -F "mkvmerge v74.0.0"', dockerfile)
        self.assertIn("PLURX_DOVI_TOOL=/usr/local/bin/dovi_tool", dockerfile)
        self.assertIn("PLURX_MKVMERGE=/usr/bin/mkvmerge", dockerfile)

        workflow = read(".github/workflows/ci.yml")
        self.assertIn("docker/setup-qemu-action@v3", workflow)
        self.assertIn("name: package and smoke (${{ matrix.arch }})", workflow)
        self.assertIn("- arch: arm64", workflow)
        self.assertIn("platforms: linux/${{ matrix.arch }}", workflow)
        self.assertIn("file: Dockerfile.release", workflow)

    def test_docker_build_keeps_cluster_validation_features_out_of_plurxd(self):
        dockerfile = read("Dockerfile")
        release = read(".github/workflows/publish-release.yml")
        self.assertNotIn(
            "cargo build --release -p plurxd -p plurx-cluster-check",
            dockerfile,
        )
        self.assertRegex(
            dockerfile,
            r"cargo build(?: --locked)? --release -p plurxd",
        )
        self.assertRegex(
            dockerfile,
            r"cargo build(?: --locked)? --release -p plurx-cluster-check",
        )
        self.assertIn("cargo tree --locked -p plurxd -e features", dockerfile)
        self.assertIn("grep -q 'cluster-read-cost-validation'", dockerfile)
        self.assertIn("CARGO_TARGET_DIR=/src/target-plurxd", dockerfile)
        self.assertIn("CARGO_TARGET_DIR=/src/target-cluster-check", dockerfile)
        self.assertIn("id=plurx-cargo-registry,sharing=locked", dockerfile)
        self.assertIn("id=plurx-target-plurxd-${TARGETARCH},sharing=locked", dockerfile)
        self.assertIn(
            "id=plurx-target-cluster-check-${TARGETARCH},sharing=locked",
            dockerfile,
        )
        self.assertIn("--binary-export", release)
        self.assertIn("target: release-binaries", release)
        self.assertIn("trusted-packaging/scripts/release-package-candidate", release)

    def test_ship_routes_real_mobile_targets_through_ansible(self):
        ship = read("scripts/ship")
        project = read("clients/apple/project.yml")
        android = read("clients/android/app/build.gradle.kts")
        subprocess.run(["bash", "-n", str(ROOT / "scripts/ship")], check=True)
        self.assertIn("plurx-iOS:", project)
        self.assertIn("plurx-tvOS:", project)
        self.assertIn('"$ANSIBLE_DIR/mobile.yml"', ship)
        self.assertIn('--tags "$MOBILE_TAGS"', ship)
        self.assertNotIn("deploy/ansible", ship)
        self.assertIn("alias(libs.plugins.android.application)", android)
        self.assertIn("deploy/docker-compose.override.yml or deploy/.env", ship)
        self.assertNotIn("environment: block in deploy/docker-compose.yml", ship)

        deploy_readme = read("deploy/README.md")
        self.assertIn("There is no direct-install step for the fleet", deploy_readme)

    def test_ship_physical_is_a_self_contained_device_path(self):
        script = read("scripts/ship-physical")
        subprocess.run(
            ["bash", "-n", str(ROOT / "scripts/ship-physical")], check=True
        )
        self.assertTrue(os.access(ROOT / "scripts/ship-physical", os.X_OK))

        # It builds for real hardware, in the same shape the Ansible role does.
        self.assertIn("-scheme plurx-iOS", script)
        self.assertIn("-scheme plurx-tvOS", script)
        self.assertIn("generic/platform=iOS", script)
        self.assertIn("generic/platform=tvOS", script)
        self.assertIn("-configuration Release", script)
        self.assertIn(":app:assembleDebug", script)

        # Neither artifact reaches a device unverified.
        self.assertIn("codesign --verify --deep --strict", script)
        self.assertIn('"$APKSIGNER" verify', script)

        # TestFlight stays an Ansible-only path; this one never uploads.
        self.assertNotIn("exportArchive", script)
        self.assertNotIn("ASC_KEY_ID", script)

        # A downgrade retains /data and hands older code a newer schema.
        self.assertIn("install --no-streaming -r", script)
        self.assertNotIn("adb -s \"$serial\" install -d", script)
        self.assertNotIn("--allow-downgrade", script)

        # macOS ships bash 3.2 — no mapfile, no associative arrays.
        self.assertNotIn("mapfile", script.split("# Not `mapfile`")[0])
        self.assertNotIn("declare -A", script)

        result = subprocess.run(
            [str(ROOT / "scripts/ship-physical"), "--help"],
            cwd=ROOT,
            check=True,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
        )
        self.assertIn("--dry-run", result.stdout)
        self.assertIn("--allow-unknown", result.stdout)

    def test_publishing_documents_the_standalone_device_path(self):
        publishing = read("docs/PUBLISHING.md")
        self.assertIn("scripts/ship-physical", publishing)
        self.assertIn("When the controller cannot run the play", publishing)

    def test_ship_has_no_obsolete_nuc4_port_exception(self):
        ship = read("scripts/ship")
        self.assertNotIn(
            "nuc4's port is held by Plex; that is accepted, not a failure",
            ship,
        )

    def test_ship_selects_mobile_tags_and_optional_vars_file(self):
        with tempfile.TemporaryDirectory() as temporary:
            environment = os.environ.copy()
            environment["PLURX_ANSIBLE_DIR"] = temporary
            environment["PLURX_ANSIBLE_INVENTORY"] = str(
                Path(temporary) / "inventory.yml"
            )
            environment["PLURX_MOBILE_VARS_FILE"] = str(
                Path(temporary) / "vars.yml"
            )
            result = subprocess.run(
                [str(ROOT / "scripts/ship"), "--apple", "--android", "--dry-run"],
                cwd=ROOT,
                env=environment,
                check=True,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
            )

        output = result.stdout
        self.assertIn("targets: apple,android", output)
        self.assertIn(f"{temporary}/mobile.yml --tags apple,android", output)
        self.assertIn(f"-e @{temporary}/vars.yml", output)

    def test_ci_provisions_concrete_apple_devices_before_testing(self):
        workflow = read(".github/workflows/ci.yml")
        makefile = read("Makefile")
        self.assertIn("/Applications/Xcode.app/Contents/Developer", workflow)
        self.assertIn("/Applications/Xcode_26.6.app/Contents/Developer", workflow)
        self.assertIn("brew install xcodegen", workflow)
        self.assertIn('grep -Fxq "Xcode 26.6"', workflow)
        self.assertIn('grep -Fxq "Build version 17F113"', workflow)
        self.assertIn('= "Version: 2.46.0"', workflow)
        self.assertIn('iOS 26.5 (26.5 - 23F77)', workflow)
        self.assertIn('tvOS 26.5 (26.5 - 23L470)', workflow)
        self.assertNotIn("sudo xcode", workflow)
        self.assertEqual(workflow.count("xcrun simctl create"), 3)
        self.assertIn("SimDeviceType.iPhone-17-Pro", workflow)
        self.assertIn("SimDeviceType.iPad-Pro-13-inch-M4-8GB", workflow)
        self.assertEqual(workflow.count("SimRuntime.iOS-26-5"), 2)
        self.assertIn("SimDeviceType.Apple-TV-4K-3rd-generation-4K", workflow)
        self.assertIn("SimRuntime.tvOS-26-5", workflow)
        self.assertIn('APPLE_IOS_SIM=platform=iOS Simulator,id=$ios_id', workflow)
        self.assertIn('APPLE_IPAD_SIM=platform=iOS Simulator,id=$ipad_id', workflow)
        self.assertIn('APPLE_TVOS_SIM=platform=tvOS Simulator,id=$tvos_id', workflow)
        self.assertIn("name: Delete the run's simulators", workflow)
        self.assertIn("if: always()", workflow)
        self.assertIn('xcrun simctl delete "$udid"', workflow)
        self.assertIn('$${APPLE_IPAD_SIM:-}', makefile)
        self.assertLess(workflow.index("xcrun simctl create"), workflow.index("run: make apple-test"))

        # Each platform compiles once and every destination replays those
        # products: one iOS build-for-testing feeds the iPhone AND iPad
        # test-without-building runs, one tvOS build feeds Apple TV. A third
        # `test` (with building) invocation sneaking back in is the regression
        # this pins out.
        apple_target = makefile.split(".PHONY: apple-test", 1)[1].split(".PHONY:", 1)[0]
        self.assertEqual(apple_target.count("build-for-testing"), 2)
        self.assertEqual(apple_target.count("test-without-building"), 3)
        self.assertEqual(apple_target.count("-scheme plurx-iOS"), 3)
        self.assertEqual(apple_target.count("-scheme plurx-tvOS"), 2)
        self.assertNotRegex(
            apple_target, r"CODE_SIGNING_ALLOWED=NO test(?!-without-building)"
        )
        self.assertLess(
            apple_target.index("build-for-testing"),
            apple_target.index("test-without-building"),
        )
        self.assertEqual(apple_target.count('-derivedDataPath "$(APPLE_DERIVED_DATA)"'), 5)
        # The DerivedData cache in ci.yml must point at the same directory the
        # Makefile builds into, or it silently caches nothing.
        self.assertIn("APPLE_DERIVED_DATA := build/DerivedData", makefile)
        self.assertIn("path: clients/apple/build/DerivedData", workflow)

    def test_pr_ci_selects_expensive_surfaces_and_has_one_aggregate_gate(self):
        workflow = read(".github/workflows/ci.yml")

        self.assertIn("python3 -m validation.ci_scope", workflow)
        self.assertIn("name: fast policy and contract preflight", workflow)
        self.assertIn("name: mobile release version", workflow)
        self.assertIn("name: Check mobile release hygiene", workflow)
        self.assertLess(
            workflow.index("name: Audit corrective-history evidence"),
            workflow.index("name: Check validation catalog and contract unit tests"),
        )
        self.assertIn("name: Main promotion gate", workflow)
        self.assertIn("branches: [main]", workflow)
        self.assertIn("scope_event=effort_qualification", workflow)
        self.assertIn("qualification: ${{ steps.scope.outputs.qualification }}", workflow)
        fast_rust = workflow.split("  check:", 1)[1].split(
            "\n  cluster_store:", 1
        )[0]
        self.assertIn("name: fast Rust gate", fast_rust)
        self.assertIn("run: make ci-rust-gate", fast_rust)
        self.assertNotIn("scripts/validate run", fast_rust)
        self.assertNotIn("actions/setup-node", fast_rust)
        self.assertNotIn("./.github/actions/playwright", fast_rust)
        self.assertNotIn("./.github/actions/ffmpeg", fast_rust)
        self.assertIn("if: needs.scope.outputs.apple == 'true'", workflow)
        web_layout = workflow.split("\n  web_layout:", 1)[1].split(
            "\n  android_jvm:", 1
        )[0]
        android_device = workflow.split("\n  android_device:", 1)[1].split(
            "\n  coverage:", 1
        )[0]
        self.assertIn("if: needs.scope.outputs.web_layout == 'true'", web_layout)
        self.assertIn("if: needs.scope.outputs.android_device == 'true'", android_device)
        vod_web = workflow.split("\n  vod_web:", 1)[1].split(
            "\n  web_layout:", 1
        )[0]
        self.assertIn("--case suspend-resume", vod_web)
        self.assertIn("docs/VOD-STEADY-ACCEPTANCE-HANDOFF.md", vod_web)
        self.assertIn(
            "if: needs.scope.outputs.release_build == 'true' || "
            "needs.scope.outputs.container == 'true'",
            workflow,
        )
        self.assertIn("needs.scope.outputs.hiqlite_spike == 'true'", workflow)
        self.assertIn("needs.scope.outputs.cluster_auth == 'true'", workflow)
        self.assertIn("name: replicated Store contracts", workflow)
        self.assertIn("name: replicated topology contracts", workflow)
        self.assertIn("name: replicated WAL recovery contracts", workflow)
        self.assertIn("name: cluster daemon contracts", workflow)
        self.assertIn("if: needs.scope.outputs.rust == 'true'", workflow)
        self.assertIn("needs: [scope, preflight]", workflow)
        self.assertIn("PREFLIGHT_RESULT: ${{ needs.preflight.result }}", workflow)
        self.assertIn(
            "MOBILE_VERSION_RESULT: ${{ needs.mobile_version.result }}",
            workflow,
        )
        self.assertIn(
            "CLUSTER_STORE_RESULT: ${{ needs.cluster_store.result }}", workflow
        )
        self.assertIn(
            "CLUSTER_TOPOLOGY_RESULT: ${{ needs.cluster_topology.result }}",
            workflow,
        )
        self.assertIn("CLUSTER_WAL_RESULT: ${{ needs.cluster_wal.result }}", workflow)
        self.assertIn(
            "CLUSTER_DAEMON_RESULT: ${{ needs.cluster_daemon.result }}", workflow
        )
        pr_gate = workflow.split("  pr_gate:", 1)[1]
        self.assertIn("      - mobile_version", pr_gate)
        self.assertIn("      - cluster_store", pr_gate)
        self.assertIn("      - cluster_topology", pr_gate)
        self.assertIn("      - cluster_wal", pr_gate)
        self.assertIn("      - cluster_daemon", pr_gate)
        self.assertIn("      - web_layout", pr_gate)
        self.assertIn("      - vod_web", pr_gate)
        self.assertIn("      - android_device", pr_gate)
        self.assertNotIn("      - hiqlite_spike", pr_gate)
        self.assertIn("needs: scope", workflow)
        self.assertNotIn("github.event_name == 'pull_request' && github.ref == 'refs/heads/main'", workflow)

        mobile = workflow.split("  mobile_version:", 1)[1].split("\n  preflight:", 1)[0]
        self.assertIn("needs: scope", mobile)
        self.assertNotIn("needs: [scope, preflight]", mobile)

        # The recorded base sha is the target tip at event time. Comparing build
        # counters against it keeps a branch green after an unrelated release
        # bump lands the same counter on the target, because identical literals
        # auto-merge with no conflict. The counter baseline must be re-fetched
        # at job time; the recorded base stays, but only to scope the diff.
        self.assertIn("+refs/heads/$BASE_REF:refs/remotes/origin/$BASE_REF", mobile)
        self.assertIn('MERGE_TARGET=origin/$BASE_REF" >> "$GITHUB_ENV"', mobile)
        self.assertIn('PLURX_VALIDATION_MERGE_TARGET="$MERGE_TARGET"', mobile)
        self.assertIn(
            "BASE_REF: ${{ github.event.pull_request.base.ref }}",
            mobile,
        )
        self.assertLess(
            mobile.index("Resolve the current merge target"),
            mobile.index("Check mobile release hygiene"),
        )
        apple = workflow.split("\n  apple:\n", 1)[1].split(
            "\n  android_device:\n", 1
        )[0]
        self.assertIn("needs: [scope, preflight]", apple)
        self.assertNotIn("mobile_version", apple)

        coverage = workflow.split("  coverage:", 1)[1].split("\n  build:", 1)[0]
        self.assertIn("if: github.ref == 'refs/heads/main'", coverage)
        self.assertIn("--failure-mode all", coverage)
        # Instrumenting the cluster harness made the diagnostic badge depend on
        # replicated-store deadlines and turned one slow worker into a red CI
        # badge. Keep coverage on the same runner-neutral lane as `check`.
        self.assertIn(
            "cargo llvm-cov --workspace --locked --exclude plurx-cluster-check",
            coverage,
        )
        self.assertIn("git add coverage.json coverage.svg", coverage)

        # Shields cannot fetch badge data anonymously from a private branch.
        # GitHub serves both badge images to authorized repository viewers.
        readme = read("README.md")
        self.assertIn(
            "ci.yml/badge.svg?branch=main&event=push",
            readme,
        )
        self.assertIn("blob/badges/coverage.svg?raw=true", readme)
        self.assertNotIn("img.shields.io/endpoint", readme)
        self.assertNotIn("raw.githubusercontent.com/pjunod/plurx/badges", readme)

        package = workflow_job_blocks(".github/workflows/ci.yml")["package_smoke"]
        self.assertNotIn("needs: check", package)
        self.assertIn("needs: [scope, preflight]", package)

        preflight = workflow_job_blocks(".github/workflows/ci.yml")["preflight"]
        effort_preflight = workflow_job_blocks(".github/workflows/effort-ci.yml")[
            "preflight"
        ]
        for contract_preflight in (preflight, effort_preflight):
            self.assertIn("uses: actions/setup-node@v4", contract_preflight)
            self.assertIn('node-version: "22"', contract_preflight)
            self.assertLess(
                contract_preflight.index("actions/setup-node@v4"),
                contract_preflight.index("run: make operations-check"),
            )

        lint = read(".github/workflows/lint.yml")
        self.assertNotIn("\n  pull_request:\n", lint)
        self.assertNotIn("\n  merge_group:\n", lint)
        self.assertIn("workflow_dispatch:", lint)
        self.assertIn("run: make fmt-check lint", lint)

        self.assertIn("target: release-binaries", package)
        self.assertIn("scripts/release-package-candidate", package)
        self.assertNotIn("actions/download-artifact", package)
        # Root container lanes still install ffmpeg through apt. Keep that one
        # package-manager boundary on the canonical archive with retries;
        # persistent runner jobs consume dependencies provisioned by Ansible.
        action = read(".github/actions/ffmpeg/action.yml")
        self.assertIn(
            "s|mirror+file:/etc/apt/apt-mirrors.txt"
            "|https://archive.ubuntu.com/ubuntu|g",
            action,
        )
        self.assertEqual(
            [],
            [
                line.strip()
                for line in action.splitlines()
                if "apt-get" in line
                and not line.lstrip().startswith("#")
                and "-o Acquire::Retries=3" not in line
            ],
            ".github/actions/ffmpeg/action.yml runs apt-get without retries",
        )
        self.assertNotIn("sudo apt-get update", workflow)
        self.assertNotIn("sudo apt-get install", workflow)
        self.assertNotIn(
            "cargo build --release --workspace --target ${{ matrix.target }}",
            workflow,
        )

    def test_main_qualification_is_full_and_effort_prs_are_compile_only(self):
        workflow = read(".github/workflows/ci.yml")
        effort = read(".github/workflows/effort-ci.yml")
        lint = read(".github/workflows/lint.yml")
        makefile = read("Makefile")
        precommit = read("scripts/pre-commit")

        # The single required aggregate workflow must fire on merge_group.
        # The badge-only lint workflow runs after merge; Clippy already belongs
        # to the aggregate workflow's fast Rust lane.
        self.assertIn("\n  merge_group:\n", workflow)
        self.assertNotIn("\n  merge_group:\n", lint)
        self.assertNotIn("\n  pull_request:\n", lint)
        self.assertIn(
            "if: always() && (github.event_name == 'pull_request' || github.event_name == 'merge_group')",
            workflow,
        )
        # Queue runs must never be cancelled by a later PR push.
        self.assertIn(
            "github.event_name == 'push' && github.ref == 'refs/heads/main'",
            workflow,
        )

        # Task PRs target effort/** and get one always-present aggregate. The
        # lane compiles every affected language but executes none of the slow
        # release suites; an effort/** -> main PR is expanded above instead.
        self.assertIn('      - "effort/**"', effort)
        self.assertIn("name: Effort development gate", effort)
        self.assertIn("run: make effort-rust-check", effort)
        self.assertIn("run: make apple-build", effort)
        self.assertIn("run: make android", effort)
        self.assertIn("run: make web-check", effort)
        self.assertNotIn("make ci-rust-gate", effort)
        self.assertNotIn("make cluster-", effort)
        self.assertNotIn("make apple-test", effort)
        self.assertNotIn("make android-test", effort)
        self.assertNotIn("android-instrumentation", effort)
        self.assertNotIn("ui-check", effort)
        self.assertNotIn("playback-lab", effort)
        self.assertNotIn("docker/build-push-action", effort)
        self.assertIn("$(CARGO) check --workspace --locked --all-targets", makefile)
        self.assertIn('"${PLURX_EFFORT_COMMIT:-}" = "1"', precommit)
        self.assertIn("ROOT=$(git rev-parse --show-toplevel)", precommit)
        self.assertIn(
            "make history-check validation-lint operations-check effort-rust-check",
            precommit,
        )
        self.assertIn("scripts/validate run --profile commit --staged", precommit)

        # Exactly five Rust test lanes own the PR: the fast gate plus four
        # independently selected Store/topology/WAL/daemon jobs.
        gate = makefile.split(".PHONY: ci-rust-gate", 1)[1].split(".PHONY:", 1)[0]
        self.assertIn("--workspace --locked --exclude plurx-cluster-check", gate)
        # The lockfile check sits in front of Clippy on purpose. `spikes/
        # hiqlite-m0` is a separate workspace that path-depends on plurx-core,
        # so adding a dependency to plurx-core strands its lockfile — and the
        # thing that used to notice was `cargo clippy --locked` in a different
        # job, twenty minutes in. Running it first costs under a second and
        # fails with the same message.
        self.assertIn("ci-rust-gate: fmt-check spike-lock-check lint", gate)
        self.assertIn("spike-lock-check", makefile.split("\nunit:", 1)[1].split("\n\n", 1)[0])
        self.assertIn("run: make ci-rust-gate", workflow)
        self.assertIn("make fmt-check lint", lint)
        jobs = workflow_job_blocks(".github/workflows/ci.yml")
        self.assertIn("make cluster-store-check", jobs["cluster_store"])
        self.assertNotIn("make cluster-harness-check", jobs["cluster_store"])
        self.assertIn("make cluster-harness-check", jobs["cluster_topology"])
        self.assertNotIn("make cluster-store-check", jobs["cluster_topology"])
        self.assertIn("run: make cluster-wal-check", workflow)
        self.assertIn("run: make cluster-daemon-check", workflow)

    def test_ci_caches_are_keyed_to_what_they_cache(self):
        workflow = read(".github/workflows/ci.yml")
        action = read(".github/actions/playwright/action.yml")

        # The Playwright pip pin and the browser-bundle cache key must move
        # together, or a version bump silently reuses the wrong browsers.
        action_pin = re.search(r'PLAYWRIGHT_VERSION: \$\{\{ inputs\.version \}\}', action)
        workflow_pin = re.search(
            r"uses: \./\.github/actions/playwright\n\s+with:\n\s+version: \"(\d+\.\d+\.\d+)\"",
            workflow,
        )
        cache_pin = re.search(r"playwright-\$\{\{ runner\.os \}\}-(\d+\.\d+\.\d+)", workflow)
        self.assertIsNotNone(action_pin)
        self.assertIsNotNone(workflow_pin)
        self.assertIsNotNone(cache_pin)
        self.assertEqual(workflow_pin.group(1), cache_pin.group(1))

        # Both Android jobs reuse the GHCR toolchain image keyed on the
        # Dockerfile hash, and the Makefile honors the pre-pull instead of
        # rebuilding the SDK image from scratch.
        self.assertEqual(
            workflow.count("sha256sum clients/android/Dockerfile"), 2
        )
        self.assertEqual(workflow.count("PLURX_ANDROID_IMAGE_READY=1"), 4)
        makefile = read("Makefile")
        self.assertIn('if [ "$${PLURX_ANDROID_IMAGE_READY:-}" = "1" ]', makefile)

        # A restored AVD made the Compose focus suite fail after the same
        # emulator passed from a cold image. Keep the SDK outside the checkout
        # (so post-job hashFiles cannot traverse it) and build a disposable AVD
        # for every run instead of treating the snapshot as portable state.
        self.assertIn('echo "ANDROID_HOME=$RUNNER_TEMP/android-sdk"', workflow)
        self.assertIn('echo "ANDROID_SDK_ROOT=$RUNNER_TEMP/android-sdk"', workflow)
        self.assertNotIn("name: Cache the AVD snapshot", workflow)
        self.assertIn("force-avd-creation: true", workflow)
        self.assertIn("-no-snapshot-save", workflow)
        self.assertIn("api-level: 36", workflow)
        self.assertIn("target: android-tv", workflow)
        self.assertIn("arch: x86", workflow)
        self.assertNotIn("arch: x86_64", workflow)
        self.assertIn("profile: tv_1080p", workflow)
        self.assertNotIn("clients/android/**/*.gradle*", workflow)
        self.assertEqual(workflow.count("clients/android/app/build.gradle.kts"), 2)
        android_device = workflow.split("  android_device:", 1)[1].split(
            "\n  coverage:", 1
        )[0]
        self.assertEqual(
            android_device.count("uses: reactivecircus/android-emulator-runner@v2"),
            2,
        )
        android_steps = workflow_step_blocks(android_device)
        first_attempt = android_steps[
            "Run the Compose and TV focus suite on a disposable emulator"
        ]
        freeze_attempt = android_steps[
            "Freeze whether the first instrumentation script started"
        ]
        retry_attempt = android_steps["Retry a pre-test emulator boot failure once"]
        propagate_failure = android_steps[
            "Propagate an instrumented-test failure without retrying it"
        ]

        self.assertEqual(
            workflow_step_scalar(first_attempt, "id"), "android_instrumentation"
        )
        self.assertEqual(
            workflow_step_scalar(first_attempt, "continue-on-error"), "true"
        )
        first_script_marker = "          script: |\n"
        self.assertEqual(first_attempt.count(first_script_marker), 1)
        self.assertEqual(
            first_attempt.split(first_script_marker, 1)[1],
            "            mkdir -p target/validation\n"
            "            touch target/validation/android-instrumentation-started\n"
            '            PLURX_ANDROID_SERIAL="emulator-${EMULATOR_PORT}" '
            "make android-instrumentation-run\n",
        )
        self.assertEqual(
            workflow_step_scalar(freeze_attempt, "id"),
            "android_instrumentation_first_attempt",
        )
        self.assertEqual(workflow_step_scalar(freeze_attempt, "if"), "always()")
        freeze_script_marker = "        run: |\n"
        self.assertEqual(freeze_attempt.count(freeze_script_marker), 1)
        self.assertEqual(
            freeze_attempt.split(freeze_script_marker, 1)[1],
            "          if [[ -f "
            "target/validation/android-instrumentation-started ]]; then\n"
            '            echo "started=true" >> "$GITHUB_OUTPUT"\n'
            "          else\n"
            '            echo "started=false" >> "$GITHUB_OUTPUT"\n'
            "          fi\n",
        )
        self.assertEqual(
            workflow_step_scalar(retry_attempt, "if"),
            "steps.android_instrumentation.outcome == 'failure' && "
            "steps.android_instrumentation_first_attempt.outputs.started == 'false'",
        )
        self.assertNotIn("continue-on-error:", retry_attempt)
        retry_script_marker = "          script: |\n"
        self.assertEqual(retry_attempt.count(retry_script_marker), 1)
        self.assertEqual(
            retry_attempt.split(retry_script_marker, 1)[1],
            "            mkdir -p target/validation\n"
            "            touch target/validation/android-instrumentation-started\n"
            '            PLURX_ANDROID_SERIAL="emulator-${EMULATOR_PORT}" '
            "make android-instrumentation-run\n",
        )
        self.assertEqual(
            workflow_step_scalar(propagate_failure, "if"),
            "steps.android_instrumentation.outcome == 'failure' && "
            "steps.android_instrumentation_first_attempt.outputs.started == 'true'",
        )
        self.assertEqual(workflow_step_scalar(propagate_failure, "run"), "exit 1")
        self.assertNotIn(
            "hashFiles('target/validation/android-instrumentation-started')",
            android_device,
        )
        self.assertLess(
            android_device.index("id: android_instrumentation_first_attempt"),
            android_device.index("Retry a pre-test emulator boot failure once"),
        )
        self.assertIn("uninstall tv.plurx.app.test", makefile)
        self.assertIn("uninstall tv.plurx.app", makefile)
        self.assertIn("target/validation/android-instrumentation.txt", makefile)
        self.assertIn("Android instrumentation did not report a passing suite", makefile)

        # Store semantics and topology have independent caches, routing,
        # failure logs, and exact-tree receipts.
        jobs = workflow_job_blocks(".github/workflows/ci.yml")
        store = jobs["cluster_store"]
        topology = jobs["cluster_topology"]
        wal = workflow.split("  cluster_wal:", 1)[1].split(
            "\n  cluster_daemon:", 1
        )[0]
        daemon = workflow.split("  cluster_daemon:", 1)[1].split(
            "\n  web_layout:", 1
        )[0]
        self.assertIn("uses: ./.github/actions/cargo-cache", store)
        self.assertIn("lane: cluster-store", store)
        self.assertIn('persistent-eligible: "true"', store)
        self.assertIn("uses: ./.github/actions/cargo-cache", topology)
        self.assertIn("lane: cluster-topology", topology)
        self.assertIn('persistent-eligible: "true"', topology)
        self.assertIn(
            "Resolve pinned Rust executables for the long contract run", topology
        )
        self.assertIn("rustup which --toolchain 1.97.1 cargo", topology)
        self.assertIn("rustup which --toolchain 1.97.1 rustc", topology)
        self.assertNotIn("CARGO: rustup run 1.97.1 cargo", topology)
        self.assertIn("make cluster-store-check", store)
        self.assertNotIn("make cluster-harness-check", store)
        self.assertIn("make cluster-harness-check", topology)
        self.assertNotIn("make cluster-store-check", topology)
        self.assertIn("make hiqlite-spike", topology)
        self.assertIn("cluster-store-receipt.json", store)
        self.assertIn("cluster-topology-receipt.json", topology)
        self.assertIn("if: always()", store)
        self.assertIn("if: always()", topology)
        self.assertIn("run: make cluster-wal-check", wal)
        self.assertIn("run: make cluster-daemon-check", daemon)
        self.assertIn("name: Verify the cluster fixture generator", daemon)
        self.assertIn("command -v ffmpeg", daemon)
        self.assertNotIn("spikes/hiqlite-m0/target", workflow)
        self.assertIn("name: cluster-topology-semantic", topology)
        self.assertIn(
            "path: target/validation/cluster-topology-semantic.json", topology
        )
        self.assertIn("if-no-files-found: error", topology)

        # Hosted smoke keeps scoped GHA state; an eligible self-hosted smoke
        # uses the one named host builder and enforces its postcondition.
        package = workflow_job_blocks(".github/workflows/ci.yml")["package_smoke"]
        self.assertIn("uses: ./.github/actions/buildx-cache", package)
        self.assertIn("type=gha,scope=package-compile-{0}", package)
        self.assertIn("type=gha,mode=max,scope=package-compile-{0}", package)
        self.assertIn("type=gha,scope=package-runtime-{0}", package)
        self.assertIn("type=gha,mode=min,scope=package-runtime-{0}", package)
        self.assertIn("PLURX_BUILD_SHA=${{ github.sha }}", package)
        self.assertIn("plurx-cluster-check", package)
        self.assertIn("build-identity", package)
        self.assertIn(
            'run: scripts/ci-buildkit-prune "$BUILDER_NAME" 50', package
        )

    def test_hiqlite_shutdown_budget_covers_its_deliberate_cluster_waits(self):
        management = read("vendor/hiqlite/src/client/mgmt.rs")
        signal_handler = read("vendor/hiqlite/src/client/shutdown_handle.rs")

        self.assertIn(
            "RAFT_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30)",
            management,
        )
        self.assertIn("tokio::time::timeout(\n                RAFT_SHUTDOWN_TIMEOUT", management)
        self.assertIn("use super::mgmt::RAFT_SHUTDOWN_TIMEOUT", signal_handler)
        self.assertIn("time::timeout(\n            RAFT_SHUTDOWN_TIMEOUT", signal_handler)

    def test_ci_artifacts_are_bounded_and_pr_builds_do_not_retain_binaries(self):
        workflow = read(".github/workflows/ci.yml")

        # Every artifact has an explicit bound. Only the tiny qualification
        # receipt outlives the one-day diagnostic binaries.
        self.assertEqual(
            workflow.count("uses: actions/upload-artifact@v4"),
            workflow.count("retention-days:"),
        )
        self.assertIn("retention-days: 14", workflow)

        # Ordinary PRs retain only the small identity/digest receipt. Pushes
        # and final qualifications retain exact binaries for one day.
        build = workflow_job_blocks(".github/workflows/ci.yml")["package_smoke"]
        self.assertIn(
            "name: Retain candidate binaries for push, tag, and qualification runs",
            build,
        )
        self.assertIn("needs.scope.outputs.qualification == 'true'", build)
        self.assertIn("name: Retain the exact package receipt", build)
        self.assertIn("release-bin/*.sha256", build)
        self.assertIn(
            "continue-on-error: ${{ needs.scope.outputs.qualification != 'true' }}",
            build,
        )
        self.assertIn("name: package-smoke-binaries-${{ matrix.arch }}", build)
        self.assertIn("retention-days: 1", build)
        self.assertIn("retention-days: 14", build)

        gate = workflow.split("  pr_gate:", 1)[1]
        self.assertIn("python3 -m validation.qualification", gate)
        self.assertIn("qualification-receipt.json", gate)
        self.assertIn(
            "name: effort-qualification-${{ github.event.pull_request.number }}",
            gate,
        )

    def test_release_registry_and_weekly_readiness_match_ci(self):
        ci = read(".github/workflows/ci.yml")
        publisher = read(".github/workflows/publish-release.yml")
        unraid = read("deploy/unraid-plurx.xml")
        readiness = read(".github/workflows/release-readiness.yml")

        self.assertIn("uses: ./.github/workflows/publish-release.yml", ci)
        self.assertIn("REGISTRY_IMAGE: ghcr.io/${{ github.repository }}", publisher)
        self.assertIn("<Repository>ghcr.io/pjunod/plurx:latest</Repository>", unraid)
        self.assertIn(
            "<Registry>https://github.com/pjunod/plurx/pkgs/container/plurx</Registry>",
            unraid,
        )
        self.assertIn('cron: "41 16 * * 1"', readiness)
        self.assertIn("run: make release-check", readiness)
        self.assertIn("fetch-depth: 0", readiness)

    def test_every_actions_job_has_an_explicit_timeout(self):
        for path in (
            ".github/workflows/ci.yml",
            ".github/workflows/effort-ci.yml",
            ".github/workflows/lint.yml",
            ".github/workflows/publish-release.yml",
            ".github/workflows/release-readiness.yml",
            ".github/workflows/rust-audit.yml",
        ):
            with self.subTest(path=path):
                jobs = workflow_job_blocks(path)
                self.assertTrue(jobs, f"{path} has no jobs")
                missing = [
                    name for name, block in jobs.items()
                    if "\n    timeout-minutes:" not in block
                    and "\n    uses:" not in block
                ]
                self.assertEqual([], missing, f"jobs without timeouts in {path}")

    def test_rust_audit_can_report_informational_advisories(self):
        workflow = read(".github/workflows/rust-audit.yml")
        permissions = workflow.split("permissions:\n", 1)[1].split("\njobs:\n", 1)[0]
        jobs = workflow_job_blocks(".github/workflows/rust-audit.yml")

        self.assertEqual(permissions, "  contents: read\n")
        for name in ("workspace", "fuzz"):
            with self.subTest(name=name):
                self.assertIn("if: github.event_name != 'schedule'", jobs[name])
                self.assertIn("      checks: write", jobs[name])
                self.assertNotIn("      issues: write", jobs[name])
        scheduled = jobs["scheduled"]
        self.assertIn("if: github.event_name == 'schedule'", scheduled)
        self.assertIn("      issues: write", scheduled)
        self.assertNotIn("      checks: write", scheduled)
        self.assertEqual(scheduled.count("uses: rustsec/audit-check@"), 1)
        self.assertIn("--additional-lock fuzz/Cargo.lock", scheduled)
        self.assertIn("working-directory: target/rust-audit", scheduled)
        self.assertEqual(workflow.count("token: ${{ secrets.GITHUB_TOKEN }}"), 3)

    def test_ci_jobs_use_the_intended_runner_trust_boundary(self):
        def choose(hosted, labels):
            return (
                "    runs-on: ${{ fromJSON(vars.CI_RUNNER_MODE == 'github' && "
                + f"'[\"{hosted}\"]' || '[\"self-hosted\",{labels}]') "
                + "}}"
            )

        general = choose(
            "ubuntu-24.04", '\"Linux\",\"X64\",\"lab\",\"general\"'
        )
        high_cpu = choose(
            "ubuntu-24.04",
            '\"Linux\",\"X64\",\"lab\",\"general\",\"high-cpu\"',
        )
        ffmpeg6 = choose(
            "ubuntu-24.04",
            '\"Linux\",\"X64\",\"lab\",\"general\",\"ffmpeg-6\"',
        )
        high_cpu_ffmpeg6 = (
            choose(
                "ubuntu-24.04",
                (
                    '\"Linux\",\"X64\",\"lab\",\"general\",'
                    '\"high-cpu\",\"ffmpeg-6\"'
                ),
            )
        )
        android = choose(
            "ubuntu-24.04", '\"Linux\",\"X64\",\"lab\",\"android-kvm\"'
        )
        apple = choose(
            "macos-26",
            '\"macOS\",\"ARM64\",\"lab\",\"apple\",\"xcode-26\"',
        )
        ci_store = choose(
            "ubuntu-24.04", '\"Linux\",\"X64\",\"lab\",\"ci-store\"'
        )
        ci_topology = choose(
            "ubuntu-24.04", '\"Linux\",\"X64\",\"lab\",\"ci-topology\"'
        )
        hosted_linux_24 = "    runs-on: ubuntu-24.04"
        hosted_linux = "    runs-on: ubuntu-latest"

        for path in (
            ".github/workflows/ci.yml",
            ".github/workflows/effort-ci.yml",
            ".github/workflows/fix-evidence.yml",
            ".github/workflows/lint.yml",
            ".github/workflows/release-readiness.yml",
            ".github/workflows/rust-audit.yml",
            ".github/workflows/validation-nightly.yml",
        ):
            for name, block in workflow_job_blocks(path).items():
                runs_on = re.search(r"(?m)^    runs-on: .+$", block)
                if runs_on is None:
                    self.assertIn("\n    uses:", block, f"{path}:{name} has no runner")
                    continue
                expected = general
                if (
                    path == ".github/workflows/ci.yml" and name == "apple"
                ) or (
                    path == ".github/workflows/effort-ci.yml"
                    and name == "apple_compile"
                ):
                    expected = apple
                elif path == ".github/workflows/ci.yml" and name in {
                    "cluster_daemon",
                    "coverage",
                }:
                    expected = high_cpu_ffmpeg6
                elif path == ".github/workflows/ci.yml" and name == "web_layout":
                    expected = ffmpeg6
                elif path == ".github/workflows/ci.yml" and name == "cluster_store":
                    expected = ci_store
                elif path == ".github/workflows/ci.yml" and name == "cluster_topology":
                    expected = ci_topology
                elif path == ".github/workflows/ci.yml" and name in {
                    "check",
                    "cluster_wal",
                    "package_smoke",
                }:
                    expected = high_cpu
                elif (
                    path == ".github/workflows/effort-ci.yml"
                    and name == "rust_compile"
                ):
                    expected = high_cpu
                elif path == ".github/workflows/ci.yml" and name in {
                    "android_jvm",
                    "android_device",
                }:
                    expected = android
                elif (
                    path == ".github/workflows/effort-ci.yml"
                    and name == "android_compile"
                ):
                    expected = android
                elif (
                    path == ".github/workflows/validation-nightly.yml"
                    and name == "ffmpeg8-pacing"
                ):
                    expected = hosted_linux_24
                elif (
                    "uses: ./.github/actions/ffmpeg" in block
                    and "container: ubuntu:26.04" not in block
                ):
                    expected = ffmpeg6
                self.assertEqual(expected, runs_on.group(0), f"{path}:{name}")

        for path in (".github/workflows/publish-release.yml",):
            for name, block in workflow_job_blocks(path).items():
                runs_on = re.search(r"(?m)^    runs-on: .+$", block)
                self.assertIsNotNone(runs_on, f"{path}:{name} has no runner")
                self.assertEqual(hosted_linux, runs_on.group(0), f"{path}:{name}")

        for name, block in workflow_job_blocks(
            ".github/workflows/rust-audit.yml"
        ).items():
            self.assertIn(
                "uses: dtolnay/rust-toolchain@1.97.1",
                block,
                f"rust-audit:{name} does not provision the pinned Cargo toolchain",
            )

    def test_job_containers_never_use_persistent_self_hosted_workspaces(self):
        workflow_root = ROOT / ".github/workflows"
        for path in sorted(workflow_root.glob("*.y*ml")):
            relative = path.relative_to(ROOT).as_posix()
            for name, block in workflow_job_blocks(relative).items():
                if "\n    container:" not in block:
                    continue
                runs_on = re.search(r"(?m)^    runs-on: .+$", block)
                self.assertIsNotNone(runs_on, f"{relative}:{name} has no runner")
                self.assertNotIn(
                    "self-hosted",
                    runs_on.group(0),
                    f"{relative}:{name} runs a root container on persistent storage",
                )

    def test_ci_runner_mode_has_one_validated_operator_switch(self):
        ci = read(".github/workflows/ci.yml")
        audit = read(".github/workflows/rust-audit.yml")
        script = ROOT / "scripts/ci-runner-mode"

        self.assertIn("CI_RUNNER_MODE must be self-hosted or github", ci)
        self.assertIn("vars.CI_RUNNER_MODE == 'github'", ci)
        self.assertEqual(audit.count("vars.CI_RUNNER_MODE == 'github'"), 3)
        self.assertTrue(script.stat().st_mode & 0o111)
        subprocess.run(
            [str(script), "--help"],
            check=True,
            stdout=subprocess.PIPE,
            text=True,
        )
        switch = script.read_text()
        self.assertIn("gh variable set CI_RUNNER_MODE", switch)
        self.assertIn("gh variable list", switch)
        self.assertNotIn("2>/dev/null", switch)
        self.assertIn("new workflow runs will use", switch)
        self.assertIn("self-hosted|github", switch)

    def test_every_ffmpeg_lane_pins_the_build_it_asserts_against(self):
        # An unpinned `apt-get install -y ffmpeg` on `ubuntu-latest` made the
        # gate's ffmpeg whichever build GitHub promoted that week — which is why
        # ffmpeg 8 dropping the `-readrate_initial_burst` behaviour (#380) was
        # invisible to CI and would have arrived on `main` as a green-to-red
        # flip with no diff to blame. The pin is only worth as much as this
        # test: it is what keeps a change of ffmpeg build a reviewable diff.
        action = read(".github/actions/ffmpeg/action.yml")
        self.assertIn("apt-get -o Acquire::Retries=3 install -y ffmpeg", action)
        self.assertIn("WANT_MAJOR", action)
        self.assertIn('GITHUB_STEP_SUMMARY', action)
        self.assertIn('if [ "$got" != "$WANT_MAJOR" ]', action)

        workflow_paths = sorted(
            path.relative_to(ROOT).as_posix()
            for pattern in ("*.yml", "*.yaml")
            for path in (ROOT / ".github/workflows").glob(pattern)
        )
        # Discover workflows rather than maintaining an allowlist: a newly
        # added workflow must not be able to reintroduce a direct ffmpeg install
        # merely because this test predates it. Keep one formerly unlisted file
        # explicit as regression evidence for that failure mode.
        self.assertIn(".github/workflows/fix-evidence.yml", workflow_paths)

        majors = set()
        for path in workflow_paths:
            with self.subTest(path=path):
                self.assertNotRegex(
                    read(path),
                    r"(?m)^\s*[^#\n]*apt-get[^\n]*install[^\n]*\bffmpeg\b",
                    f"{path} installs ffmpeg outside ./.github/actions/ffmpeg",
                )
                for name, block in workflow_job_blocks(path).items():
                    if "uses: ./.github/actions/ffmpeg" not in block:
                        continue
                    major = re.search(r'(?m)^ +major: "(\d+)"$', block)
                    self.assertIsNotNone(
                        major, f"{path}:{name} provisions ffmpeg without naming a major"
                    )
                    majors.add(major.group(1))
                    # A named major is a claim about the environment, so the
                    # environment has to be nameable too: a concrete runner
                    # image, or a container tag that overrides it.
                    self.assertTrue(
                        "runs-on: ubuntu-latest" not in block
                        or "\n    container:" in block,
                        f"{path}:{name} asserts an ffmpeg major on a floating image",
                    )

        # Both sides of the #380 split stay covered: the gate lanes on the major
        # their timing assumptions were written against, and a nightly lane on
        # the one every worker host already runs. Changing this set is allowed
        # and is the point — it is how a decision to move the fleet's ffmpeg
        # arrives as a reviewed diff rather than as a Tuesday.
        self.assertEqual(
            {"6", "8"},
            majors,
            "the ffmpeg majors CI covers changed; update docs/VALIDATION.md's "
            "'Which ffmpeg the profiles assume' in the same commit",
        )

    def test_ci_flake_ledger_records_real_job_outcomes_and_durations(self):
        script = ROOT / "scripts/ci-flake-report"
        subprocess.run([str(script), "--help"], check=True, stdout=subprocess.PIPE)
        ledger = json.loads(read("validation/ci-flake-ledger.json"))

        self.assertEqual(ledger["schema"], 1)
        self.assertEqual(ledger["repository"], "pjunod/plurx")
        self.assertEqual(ledger["workflow"], "ci.yml")
        self.assertGreaterEqual(ledger["source"]["completed_runs_returned"], 1)
        self.assertTrue(ledger["jobs"])
        self.assertTrue(ledger["summary"])
        for job in ledger["jobs"]:
            self.assertIsInstance(job["conclusion"], str)
            self.assertIsInstance(job["duration_seconds"], (int, float))
            self.assertGreaterEqual(job["duration_seconds"], 0)
            self.assertIsInstance(job["timestamp_anomaly"], bool)

    def test_container_smoke_keeps_non_root_state_port_and_cleanup_contracts(self):
        smoke = read("scripts/container-smoke")
        subprocess.run(["sh", "-n", str(ROOT / "scripts/container-smoke")], check=True)
        self.assertIn('chmod 0777 "$scratch"', smoke)
        self.assertIn("trap cleanup EXIT HUP INT TERM", smoke)
        self.assertIn("--publish 127.0.0.1:0:32400", smoke)
        self.assertIn('container_user="$(docker image inspect', smoke)
        self.assertIn('if [ "$container_user" != "plurx" ]', smoke)
        self.assertIn("--user 0:0", smoke)
        self.assertIn("-c 'chmod -R a+rwX /scratch'", smoke)
        self.assertEqual(len(re.findall(r'docker port "\$name" 32400/tcp', smoke)), 2)
        self.assertIn('curl -fsS "$base/readyz"', smoke)
        self.assertIn('curl -fsS "$base/metrics"', smoke)
        self.assertIn('test "$instance_before" = "$instance_after"', smoke)

    def test_perf_report_counts_copy_video_as_a_real_session(self):
        namespace = runpy.run_path(str(ROOT / "scripts/perf-report"))
        lines = []
        logs = [
            {"message": "transcode ffmpeg args: encoder=software pipeline=cpu"},
            {"message": "copy-video HLS ffmpeg args: -c:v copy"},
        ]

        namespace["log_section"](lines.append, logs, {}, None)

        report = "\n".join(lines)
        self.assertIn("1 transcode · 1 copy-video", report)
        self.assertIn("most recent copy-video command", report)

    def test_android_player_renders_the_marker_display_label(self):
        player = read(
            "clients/android/app/src/main/java/tv/plurx/app/player/PlayerScreen.kt"
        )

        self.assertIn("Text(activeMarker.displayLabel", player)


if __name__ == "__main__":
    unittest.main()
