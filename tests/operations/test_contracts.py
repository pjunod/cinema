from __future__ import annotations

import json
import os
from pathlib import Path
import re
import runpy
import subprocess
import tempfile
import textwrap
import tomllib
import unittest


ROOT = Path(__file__).resolve().parents[2]


def read(path: str) -> str:
    return (ROOT / path).read_text(encoding="utf-8")


def make_dry_run_commands(target: str) -> list[str]:
    result = subprocess.run(
        ["make", "--no-print-directory", "-n", target],
        cwd=ROOT,
        check=True,
        text=True,
        stdout=subprocess.PIPE,
    )
    commands: list[str] = []
    continued: list[str] = []
    for raw_line in result.stdout.splitlines():
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        if line.endswith("\\"):
            continued.append(line[:-1].rstrip())
            continue
        continued.append(line)
        commands.append(" ".join(continued))
        continued = []
    if continued:
        raise AssertionError(f"unterminated recipe continuation for make {target}")
    return commands


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


def workflow_job_needs(job: str) -> tuple[str, ...]:
    inline = re.search(r"(?m)^    needs: (.+)$", job)
    if inline is not None:
        value = inline.group(1).strip()
        if value.startswith("[") and value.endswith("]"):
            return tuple(
                item.strip() for item in value[1:-1].split(",") if item.strip()
            )
        return (value,)
    block = re.search(r"(?m)^    needs:\n((?:      - [^\n]+\n)+)", job)
    if block is None:
        return ()
    return tuple(
        line.removeprefix("      - ")
        for line in block.group(1).splitlines()
    )


def workflow_step_scalar(step: str, key: str) -> str:
    values = re.findall(rf"(?m)^        {re.escape(key)}: ([^\n]+)$", step)
    if len(values) != 1:
        raise AssertionError(f"expected one scalar {key!r}, found {len(values)}")
    return values[0]


def runner_fleet() -> dict:
    with (ROOT / "validation/runner-fleet.toml").open("rb") as handle:
        return tomllib.load(handle)


def rostered_runners() -> tuple[tuple[str, frozenset[str]], ...]:
    """Each rostered runner and every label GitHub would match it against.

    `self-hosted` and the operating-system and architecture labels are added by
    GitHub, never by the ansible inventory, so the roster derives them here
    rather than repeating five read-only strings on every runner.
    """
    return tuple(
        (
            runner["name"],
            frozenset(runner["labels"]) | {"self-hosted", runner["os"], runner["arch"]},
        )
        for runner in runner_fleet()["runners"]
    )


def store_shard_count() -> int:
    """The single declared Store shard count, from the shard job's `env`."""
    return int(
        re.search(
            r'(?m)^      SHARD_COUNT: "(\d+)"$',
            read(".github/workflows/store-shards.yml"),
        ).group(1)
    )


def self_hosted_label_sets(block: str) -> list[tuple[str, ...]]:
    """Every self-hosted label set a job's `runs-on` can resolve to.

    A job either names the local labels as a YAML flow sequence or reads the
    same sequence from a matrix key. Parse the arrays that are actually there
    instead of matching label strings this test already knows.
    """
    direct = [
        tuple(part.strip().strip('"\'') for part in literal.split(","))
        for literal in re.findall(r"(?m)^ +runs-on: \[([^]]+)\]$", block)
    ]
    matrix = [
        tuple(json.loads(literal))
        for literal in re.findall(r"(?m)^ +runs_on: '(\[[^']*\])'$", block)
    ]
    return [labels for labels in (*direct, *matrix) if "self-hosted" in labels]


def workflow_step_literal(step: str, key: str) -> list[str]:
    marker = f"        {key}: |"
    lines = step.splitlines()
    try:
        start = lines.index(marker) + 1
    except ValueError as exc:
        raise AssertionError(f"expected one literal {key!r}") from exc

    value: list[str] = []
    for line in lines[start:]:
        if line and len(line) - len(line.lstrip(" ")) <= 8:
            break
        if not line:
            value.append("")
            continue
        if not line.startswith("          "):
            raise AssertionError(f"malformed literal {key!r}: {line!r}")
        value.append(line[10:])
    return value


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
            'if name in {"home", "activity", "analysis", "settings", "settings-developer", "live-tv"}:', script
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

    def test_ui_baseline_preserves_exact_periodic_route_request_counts(self):
        contract = runpy.run_path(
            str(ROOT / "scripts/ui-baseline"), run_name="ui_baseline_request_contract"
        )
        counted = contract["counted_api_calls"]

        self.assertEqual(
            counted(
                "activity",
                {},
                ["GET /api/v1/activity/detail"] * 3
                + ["GET /api/v1/cluster/nodes"],
            ),
            {
                "GET /api/v1/activity/detail": 3,
                "GET /api/v1/cluster/nodes": 1,
            },
        )
        self.assertEqual(
            counted(
                "analysis",
                {},
                ["GET /api/v1/analysis/summary"] * 2
                + ["GET /api/v1/analysis/jobs"] * 2,
            ),
            {
                "GET /api/v1/analysis/jobs": 2,
                "GET /api/v1/analysis/summary": 2,
            },
        )

    def test_ui_baseline_pins_vod_index_status_before_seeding_libraries(self):
        script = read("scripts/ui-baseline")
        seed = script.index("    def seed(self):")
        pinned = script.index('{"vod_index_mins": 0}', seed)
        libraries = script.index("for name, kind, share in LIBRARIES:", seed)

        self.assertLess(pinned, libraries)

    def test_activity_capture_observes_one_real_tick_without_hiding_extra_requests(self):
        script = read("scripts/ui-baseline")
        capture = runpy.run_path(str(ROOT / "scripts/ui-baseline"))["ACTIVITY_CAPTURE_JS"]
        web = read("crates/plurxd/src/web/index.html")
        page_timer = web.split("function setPageTimer(", 1)[1].split("\nasync function render()", 1)[0]
        production = "function setPageTimer(" + page_timer
        contract = r"""
const assert = require('node:assert/strict');
const vm = require('node:vm');
const [capture, production] = process.argv.slice(1);
for (const startupDelay of [0, 1600, 7000]) {
  for (const extraRequest of [false, true]) {
    let now = 0, next = 0;
    const timers = new Map(), calls = [];
    const context = vm.createContext({
      location: {hash: '#/activity'}, PAGE_TIMER: null, PAGE_RENDER_GENERATION: 1,
      setInterval(callback, delay, ...args) {
        const id = ++next;
        timers.set(id, {callback, delay, args, due: now + delay});
        return id;
      },
      clearInterval(id) { timers.delete(id); },
      record: value => calls.push(value),
    });
    context.window = context;
    vm.runInContext(capture + '\n' + production, context);
    vm.runInContext(`
      record('activity/detail'); // initial production render
      setPageTimer(() => record('activity/detail'), 3000);
      setInterval(() => record('unrelated'), 4000);
    `, context);
    if (extraRequest) {
      vm.runInContext("setInterval(() => record('activity/detail'), 4000)", context);
    }
    const target = startupDelay + 4500;
    while (true) {
      const ready = [...timers].filter(([, t]) => t.due <= target)
        .sort((a, b) => a[1].due - b[1].due)[0];
      if (!ready) break;
      const [id, timer] = ready;
      now = timer.due;
      timer.due += timer.delay;
      timer.callback(...timer.args);
    }
    assert.equal(context.__plurxActivityCaptureTick, true);
    assert.equal(context.PAGE_TIMER, null);
    assert.ok(calls.includes('unrelated'), 'other timers must remain real');
    const count = calls.filter(call => call === 'activity/detail').length;
    assert.equal(count === 2, !extraRequest,
      `raw request golden: delay=${startupDelay}, extra=${extraRequest}, count=${count}`);
  }
}
"""
        subprocess.run(["node", "-e", contract, capture, production], check=True)
        self.assertIn("page.add_init_script(ACTIVITY_CAPTURE_JS)", script)
        self.assertIn("ACTIVITY_DETAIL_BUSY === 0", script)

    def test_store_verdict_handles_unused_forgejo_workflow_without_weakening_required_lane(self):
        verdict = workflow_job_blocks(".github/workflows/ci.yml")["cluster_store"]
        step = workflow_step_blocks(verdict)["Select the required Store graph"]
        script = textwrap.dedent(step.split("        run: |\n", 1)[1])
        states = ("success", "skipped", "failure", "cancelled", "", "unknown")
        for mode in ("legacy", "shadow", "accelerated", "", "unknown"):
            for legacy in states:
                for shard in states:
                    expected = (
                        mode in ("legacy", "shadow")
                        and legacy == "success"
                        and shard in ("skipped", "success")
                    ) or (
                        mode == "accelerated"
                        and legacy == "skipped"
                        and shard == "success"
                    )
                    with self.subTest(mode=mode, legacy=legacy, shard=shard):
                        result = subprocess.run(
                            ["bash", "-e", "-o", "pipefail", "-c", script],
                            env={**os.environ, "EXECUTION_MODE": mode,
                                 "LEGACY_RESULT": legacy, "SHARD_RESULT": shard},
                            capture_output=True,
                            text=True,
                        )
                        self.assertEqual(result.returncode == 0, expected, result.stdout + result.stderr)

    def test_ui_baseline_reserves_distinct_http_raft_and_api_ports(self):
        script = read("scripts/ui-baseline")
        contract = runpy.run_path(
            str(ROOT / "scripts/ui-baseline"), run_name="ui_baseline_port_contract"
        )
        ports = contract["free_ports"](0, 3)

        self.assertEqual(len(ports), 3)
        self.assertEqual(len(set(ports)), 3)
        self.assertTrue(all(port > 0 for port in ports))
        self.assertIn("self.port, self.raft_port, self.api_port = free_ports", script)
        self.assertIn('raft_bind = "127.0.0.1:{self.raft_port}"', script)
        self.assertIn('api_bind = "127.0.0.1:{self.api_port}"', script)

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
            'start_period: "${PLURX_HEALTH_START_PERIOD:-5m}"', compose
        )
        self.assertIn(
            'PLURX_CLUSTER_INSTALL_SNAPSHOT_TIMEOUT_SECS: '
            '"${PLURX_CLUSTER_INSTALL_SNAPSHOT_TIMEOUT_SECS:-}"',
            compose,
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

    def _run_rollout_recipe(self, proof_exit: int) -> tuple[int, Path]:
        """Run the real `docker-up` recipe with the checker and Docker stubbed."""

        rollout = [
            line
            for line in make_dry_run_commands("docker-up")
            if "docker compose up -d --build" in line
        ][0]
        directory = Path(tempfile.mkdtemp())
        marker = directory / "compose-up-ran"
        stubs = directory / "bin"
        stubs.mkdir()
        (stubs / "python3").write_text(
            "#!/bin/sh\n"
            'case "$*" in\n'
            "  *--emit-start-period*) echo 1335s ;;\n"
            f"  *) exit {proof_exit} ;;\n"
            "esac\n",
            encoding="utf-8",
        )
        (stubs / "docker").write_text(
            "#!/bin/sh\n"
            f'printf "%s" "$PLURX_HEALTH_START_PERIOD" > "{marker}"\n',
            encoding="utf-8",
        )
        for stub in stubs.iterdir():
            stub.chmod(0o755)
        environment = dict(os.environ, PATH=f"{stubs}:{os.environ['PATH']}")
        result = subprocess.run(
            ["sh", "-c", rollout],
            cwd=ROOT,
            check=False,
            text=True,
            env=environment,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        return result.returncode, marker

    def test_a_failed_budget_proof_stops_the_rollout_before_it_touches_a_container(
        self,
    ):
        # The proof used to be a make prerequisite, so make itself guaranteed a
        # failed check stopped the deploy. It is now `&&` inside one recipe, so
        # the guarantee is shell-level and has to be exercised: a `;` here
        # would let a refused budget deploy anyway, and no assertion about the
        # recipe's text catches that.
        code, marker = self._run_rollout_recipe(proof_exit=2)
        self.assertNotEqual(code, 0)
        self.assertFalse(
            marker.exists(), "compose up ran after the budget proof failed"
        )

        code, marker = self._run_rollout_recipe(proof_exit=0)
        self.assertEqual(code, 0)
        self.assertTrue(marker.exists())
        # The period that was proved is the period that gets applied.
        self.assertEqual(marker.read_text(encoding="utf-8"), "1335s")

    def test_docker_up_preserves_override_discovery_and_stamps_the_build(self):
        commands = make_dry_run_commands("docker-up")
        command = "\n".join(commands)
        rollouts = [
            line for line in commands if "docker compose up -d --build" in line
        ]
        self.assertEqual(len(rollouts), 1)
        rollout = rollouts[0]
        self.assertTrue(rollout.startswith("cd deploy && "))
        self.assertEqual(rollout.count("--emit-start-period"), 1)
        # Derive once, prove that period, then apply the period that was
        # proved. A preflight proving a number the mutation does not use is
        # not a preflight, so the proof and the mutation must read the same
        # shell variable and must not each derive their own.
        proof = (
            'PLURX_HEALTH_START_PERIOD="$period" python3 '
            "../scripts/validate-docker-startup-budget"
        )
        self.assertIn(proof, rollout)
        self.assertLess(rollout.index("--emit-start-period"), rollout.index(proof))
        self.assertLess(
            rollout.index(proof), rollout.index("docker compose up -d --build")
        )
        self.assertIn('PLURX_HEALTH_START_PERIOD="$period" PLURX_BUILD_REF=', rollout)
        self.assertIn("PLURX_NODE_HOSTNAME=", rollout)
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

        stages = list(re.finditer(r"(?im)^[ \t]*from\b.*$", dockerfile))
        runtime_stage = re.search(
            r"(?im)^[ \t]*from[ \t]+runtime-assets[ \t]+as[ \t]+runtime[ \t]*$",
            dockerfile,
        )
        self.assertIsNotNone(runtime_stage)
        assert runtime_stage is not None
        self.assertEqual(runtime_stage.start(), stages[-1].start())
        runtime = dockerfile[runtime_stage.end() :]
        healthcheck_instructions = list(
            re.finditer(r"(?im)^[ \t]*healthcheck\b", dockerfile)
        )
        self.assertEqual(len(healthcheck_instructions), 1)
        self.assertGreater(healthcheck_instructions[0].start(), runtime_stage.end())
        healthcheck = re.search(
            r'(?im)^[ \t]*healthcheck[ \t]+--interval=(\S+)[ \t]+'
            r'--timeout=(\S+)[ \t]+'
            r'--start-period=(\S+) \\\n'
            r'[ \t]+cmd[ \t]+\["plurxd",[ \t]*"healthcheck"\][ \t]*$',
            runtime,
        )
        self.assertIsNotNone(healthcheck)
        assert healthcheck is not None
        interval, timeout, start_period = healthcheck.groups()
        self.assertEqual((interval, timeout), ("30s", "5s"))

        checker = runpy.run_path(str(ROOT / "scripts/validate-docker-startup-budget"))
        parse_duration = checker["parse_duration"]
        start_period_seconds = parse_duration(start_period, "Dockerfile start period")

        compose_start = re.search(
            r'(?m)^[ \t]*start_period:[ \t]*'
            r'"\$\{PLURX_HEALTH_START_PERIOD:-([^}]+)\}"[ \t]*$',
            compose,
        )
        self.assertIsNotNone(compose_start)
        assert compose_start is not None
        compose_start_seconds = parse_duration(
            compose_start.group(1), "Compose default start period"
        )
        self.assertEqual(compose_start_seconds, start_period_seconds)

        config_source = read("crates/plurx-core/src/config.rs")
        default_snapshot = re.search(
            r"(?m)^pub const DEFAULT_INSTALL_SNAPSHOT_TIMEOUT_SECS: u64 = "
            r"([\d_]+);$",
            config_source,
        )
        source_max_snapshot = re.search(
            r"(?m)^pub const MAX_INSTALL_SNAPSHOT_TIMEOUT_SECS: u64 = ([\d_]+);$",
            config_source,
        )
        migration_source = read("crates/plurx-core/src/cluster/migration.rs")
        phase_names = (
            "HIQLITE_HEALTH_TIMEOUT",
            "MEMBERSHIP_ADMISSION_TIMEOUT",
            "SNAPSHOT_CATCHUP_GRACE",
        )
        phases = {
            name: re.search(
                rf"(?m)^const {name}: Duration = Duration::from_secs\((\d+)\);$",
                migration_source,
            )
            for name in phase_names
        }
        self.assertIsNotNone(default_snapshot)
        self.assertIsNotNone(source_max_snapshot)
        self.assertTrue(all(value is not None for value in phases.values()))
        assert default_snapshot is not None and source_max_snapshot is not None

        # Dockerfile owns the image default. Compose exposes a paired override
        # for operators who deliberately extend the snapshot deadline.
        supported_default_startup_seconds = int(
            default_snapshot.group(1).replace("_", "")
        ) + sum(int(value.group(1)) for value in phases.values() if value)
        self.assertGreaterEqual(
            start_period_seconds, supported_default_startup_seconds
        )

        env_example = read("deploy/.env.example")
        max_snapshot = re.search(
            r"# PLURX_CLUSTER_INSTALL_SNAPSHOT_TIMEOUT_SECS=(\d+)", env_example
        )
        max_health = re.search(
            r"# PLURX_HEALTH_START_PERIOD=(\d+)([smh])", env_example
        )
        self.assertIsNotNone(max_snapshot)
        self.assertIsNotNone(max_health)
        assert max_snapshot is not None and max_health is not None
        env_max_snapshot_seconds = int(max_snapshot.group(1))
        source_max_snapshot_seconds = int(
            source_max_snapshot.group(1).replace("_", "")
        )
        self.assertEqual(env_max_snapshot_seconds, source_max_snapshot_seconds)
        max_health_seconds = parse_duration(
            "".join(max_health.groups()), ".env.example maximum health period"
        )
        supported_max_startup_seconds = source_max_snapshot_seconds + sum(
            int(value.group(1)) for value in phases.values() if value
        )
        self.assertGreaterEqual(max_health_seconds, supported_max_startup_seconds)

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
        self.assertNotIn("docker/setup-qemu-action@v3", workflow)
        self.assertIn("name: package and smoke (${{ matrix.arch }})", workflow)
        self.assertIn("- arch: arm64", workflow)
        self.assertIn(
            "runs_on: "
            "'[\"self-hosted\",\"Linux\",\"ARM64\",\"lab\",\"ci-arm64\"]'",
            workflow,
        )
        self.assertIn("platforms: linux/${{ matrix.arch }}", workflow)
        self.assertIn("file: Dockerfile.release", workflow)

        runtime_assets_marker = "FROM debian:bookworm-slim AS runtime-assets"
        runtime_image_marker = "FROM runtime-assets AS runtime"
        runtime_assets = dockerfile.index(runtime_assets_marker)
        runtime_image = dockerfile.index(runtime_image_marker)
        binary_copy = dockerfile.index(
            "COPY --from=build /plurxd /usr/local/bin/plurxd"
        )
        self.assertLess(runtime_assets, runtime_image)
        self.assertLess(runtime_image, binary_copy)
        runtime_assets_stage = dockerfile.split(runtime_assets_marker, 1)[1].split(
            runtime_image_marker, 1
        )[0]
        for assertion in (
            "dovi_sha=daf538c275f4e702219ce8eb61db28382193ac9d0126e1ef4185a88303af4485",
            "sha256sum -c -",
            'dovi_tool --version | grep -F "${DOVI_TOOL_VERSION}"',
            'mkvmerge --version | grep -F "mkvmerge v74.0.0"',
            "/usr/lib/jellyfin-ffmpeg/ffmpeg -hide_banner -bsfs",
            "/usr/lib/jellyfin-ffmpeg/ffmpeg -hide_banner -h filter=tonemapx",
        ):
            with self.subTest(runtime_asset_assertion=assertion):
                self.assertIn(assertion, runtime_assets_stage)

    def test_docker_build_keeps_cluster_validation_features_out_of_plurxd(self):
        dockerfile = read("Dockerfile")
        ci = read(".github/workflows/ci.yml")
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

        ci_package = workflow_job_blocks(".github/workflows/ci.yml")["package_smoke"]
        ci_package_steps = workflow_step_blocks(ci_package)
        compile_step = ci_package_steps[
            "Compile and export the exact candidate binaries"
        ]
        bind_step = ci_package_steps["Bind the binary set to this candidate tree"]
        self.assertIn("file: Dockerfile.binaries", compile_step)
        self.assertIn("target: release-binaries", compile_step)
        self.assertIn("scripts/release-package-candidate", bind_step)
        self.assertIn("binary-export release-bin", bind_step)
        self.assertIn("for name in plurxd plurx-cluster-check", bind_step)
        retention_step = ci_package_steps[
            "Retain candidate binaries for push, tag, and qualification runs"
        ]
        artifact_paths = retention_step.split("\n          path: |\n", 1)[1].split(
            "\n          if-no-files-found:", 1
        )[0]
        self.assertEqual(
            [line.strip() for line in artifact_paths.splitlines() if line.strip()],
            [
                "release-bin/plurxd",
                "release-bin/plurx-cluster-check",
                "release-bin/build-manifest.json",
                "release-bin/*.sha256",
            ],
        )

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
        self.assertNotIn("/Applications/Xcode_26.6.app/Contents/Developer", workflow)
        self.assertNotIn("brew install xcodegen", workflow)
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
            "\n  cluster_store_legacy:", 1
        )[0]
        self.assertIn("name: fast Rust gate", fast_rust)
        self.assertIn("run: make ci-rust-gate", fast_rust)
        self.assertNotIn("scripts/validate run", fast_rust)
        self.assertNotIn("actions/setup-node", fast_rust)
        self.assertNotIn("./.github/actions/playwright", fast_rust)
        self.assertIn("uses: ./.github/actions/ffmpeg", fast_rust)
        self.assertIn('major: "6"', fast_rust)
        # Membership alone would stay green with the step moved below the gate
        # it provisions, which is exactly the failure this contract records.
        self.assertLess(
            fast_rust.index("uses: ./.github/actions/ffmpeg"),
            fast_rust.index("run: make ci-rust-gate"),
        )
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
        self.assertIn("group: plurx-browser-heavy", web_layout)
        self.assertIn("cancel-in-progress: false", web_layout)
        self.assertIn("group: plurx-browser-heavy", vod_web)
        self.assertIn("cancel-in-progress: false", vod_web)
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
        self.assertIn(
            "PATH: /opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin",
            apple,
        )
        effort_apple = workflow_job_blocks(".github/workflows/effort-ci.yml")[
            "apple_compile"
        ]
        self.assertIn(
            "PATH: /opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin",
            effort_apple,
        )

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

        # The private Forgejo repository serves badges to authenticated local
        # viewers; no external badge proxy receives repository metadata.
        readme = read("README.md")
        self.assertIn(
            "ci.yml/badge.svg?branch=main&event=push",
            readme,
        )
        self.assertIn("/raw/branch/badges/coverage.svg", readme)
        self.assertIn("http://192.168.4.7:3000/noirr/plurx/actions", readme)
        self.assertIn(
            "git clone http://192.168.4.7:3000/noirr/plurx.git",
            readme,
        )
        self.assertNotIn("git clone https://github.com/pjunod/plurx", readme)
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
            self.assertIn("uses: https://data.forgejo.org/actions/setup-node@v4", contract_preflight)
            self.assertIn('node-version: "22"', contract_preflight)
            self.assertLess(
                contract_preflight.index("actions/setup-node@v4"),
                contract_preflight.index("run: make operations-check"),
            )
            # The shared player-input fixtures compile into no Rust and no
            # client on a fixture-only diff, so without this step a ruling
            # could be edited out of the contract with nothing to notice.
            self.assertIn(
                "node tests/playback/player-input-contract.test.js",
                contract_preflight,
            )
            self.assertIn("node tests/web/player-dom.test.js", contract_preflight)

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

        # Forgejo has no GitHub merge-queue event. The single required
        # aggregate workflow fires on main-bound pull requests, while the
        # badge-only lint workflow runs after merge.
        self.assertNotIn("\n  merge_group:\n", workflow)
        self.assertNotIn("\n  merge_group:\n", lint)
        self.assertNotIn("\n  pull_request:\n", lint)
        self.assertIn(
            "if: always() && github.event_name == 'pull_request'",
            workflow,
        )
        # Main pushes may supersede older main pushes, but tags remain durable.
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

        # Five stable Rust verdicts own the PR: the fast gate plus Store,
        # topology, WAL, and daemon. Store can internally select legacy or
        # sharded execution without changing the required verdict name.
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
        self.assertIn("make cluster-store-check", jobs["cluster_store_legacy"])
        self.assertNotIn(
            "make cluster-harness-check", jobs["cluster_store_legacy"]
        )
        self.assertIn("make cluster-harness-check", jobs["cluster_topology"])
        self.assertNotIn("make cluster-store-check", jobs["cluster_topology"])
        self.assertIn("run: make cluster-wal-check", workflow)
        wal_commands = make_dry_run_commands("cluster-wal-check")
        hiqlite_snapshot_tests = (
            "handler_coordinator_consumes_retained_reset_when_request_queue_is_full",
            "replacement_socket_drops_cancelled_request_after_consuming_reset",
            "live_request_on_stale_socket_requires_reconnect",
            "reset_interrupts_write_enqueue_under_backpressure",
            "forced_reset_cleanup_does_not_wait_for_full_writer_queue",
            "sqlite_install_snapshot_preserves_mismatch_for_offset_reset",
            "cache_install_snapshot_preserves_mismatch_for_offset_reset",
        )
        for test_name in hiqlite_snapshot_tests:
            matching = [command for command in wal_commands if test_name in command]
            self.assertEqual(len(matching), 1, test_name)
            command = matching[0]
            self.assertIn(
                "cargo test --locked --manifest-path vendor/hiqlite/Cargo.toml",
                command,
            )
            self.assertIn(
                "--no-default-features --features auto-heal,cache,macros,sqlite",
                command,
            )
            self.assertIn("--lib -- --exact", command)

        openraft_test = (
            "network::snapshot_transport::tests::"
            "test_chunked_reset_offset_if_snapshot_id_mismatch"
        )
        matching = [command for command in wal_commands if openraft_test in command]
        self.assertEqual(len(matching), 1)
        openraft_command = matching[0]
        self.assertIn(
            "cargo metadata --locked --manifest-path vendor/hiqlite/Cargo.toml",
            openraft_command,
        )
        self.assertIn('p["version"] == "0.9.25"', openraft_command)
        self.assertIn("cargo test --locked", openraft_command)
        self.assertIn('--manifest-path "$OPENRAFT_MANIFEST"', openraft_command)
        self.assertIn("--features generic-snapshot-data", openraft_command)
        self.assertIn("--lib -- --exact", openraft_command)
        self.assertIn("run: make cluster-daemon-check", workflow)

    def test_split_cluster_lanes_execute_and_propagate_the_exact_inventory(self):
        workflow = read(".github/workflows/ci.yml")
        makefile = read("Makefile")
        jobs = workflow_job_blocks(".github/workflows/ci.yml")
        store = workflow_step_blocks(jobs["cluster_store_legacy"])
        topology = workflow_step_blocks(jobs["cluster_topology"])

        store_run = store["Run replicated Store contracts"]
        self.assertEqual(workflow_step_scalar(store_run, "id"), "store_contracts")
        self.assertEqual(workflow_step_scalar(store_run, "continue-on-error"), "true")
        self.assertEqual(workflow_step_scalar(store_run, "run"), "|")
        self.assertEqual(
            workflow_step_literal(store_run, "run"),
            [
                "set -euo pipefail",
                "mkdir -p target/validation",
                "make cluster-store-check 2>&1 | tee "
                "target/validation/cluster-store.log",
            ],
        )

        store_receipt = store["Record the Store lane result"]
        self.assertEqual(workflow_step_scalar(store_receipt, "if"), "always()")
        self.assertIn(
            "LANE_RESULT: ${{ steps.store_contracts.outcome }}", store_receipt
        )
        self.assertIn('--result "$LANE_RESULT"', store_receipt)
        self.assertIn('--command "make cluster-store-check"', store_receipt)
        store_upload = store["Retain the Store lane log and receipt"]
        self.assertEqual(workflow_step_scalar(store_upload, "if"), "always()")
        self.assertIn("target/validation/cluster-store.log", store_upload)
        self.assertIn("target/validation/cluster-store-receipt.json", store_upload)
        store_propagate = store["Propagate the Store contract result"]
        self.assertEqual(
            workflow_step_scalar(store_propagate, "if"),
            "always() && steps.store_contracts.outcome != 'success'",
        )
        self.assertEqual(workflow_step_scalar(store_propagate, "run"), "exit 1")

        topology_run = topology["Run replicated topology contracts"]
        self.assertEqual(
            workflow_step_scalar(topology_run, "id"), "topology_contracts"
        )
        self.assertEqual(
            workflow_step_scalar(topology_run, "continue-on-error"), "true"
        )
        self.assertEqual(workflow_step_scalar(topology_run, "run"), "|")
        self.assertEqual(
            workflow_step_literal(topology_run, "run"),
            [
                "set -euo pipefail",
                "mkdir -p target/validation",
                "{",
                '  if [ "$RUN_CLUSTER_AUTH" = true ]; then',
                "    make cluster-harness-check",
                "  fi",
                '  if [ "$RUN_HIQLITE_SPIKE" = true ]; then',
                "    cargo clippy --locked --manifest-path "
                "spikes/hiqlite-m0/Cargo.toml \\",
                "      --tests --no-deps -- -D warnings",
                "    make hiqlite-spike",
                "  fi",
                "} 2>&1 | tee target/validation/cluster-topology.log",
            ],
        )

        topology_receipt = topology["Record the topology lane result"]
        self.assertEqual(workflow_step_scalar(topology_receipt, "if"), "always()")
        self.assertIn(
            "LANE_RESULT: ${{ steps.topology_contracts.outcome }}",
            topology_receipt,
        )
        self.assertIn('--result "$LANE_RESULT"', topology_receipt)
        for command in (
            "make cluster-harness-check",
            "cargo clippy --locked --manifest-path spikes/hiqlite-m0/Cargo.toml "
            "--tests --no-deps -- -D warnings",
            "make hiqlite-spike",
        ):
            self.assertIn(f'--command "{command}"', topology_receipt)
        topology_upload = topology["Retain the topology lane log and receipt"]
        self.assertEqual(workflow_step_scalar(topology_upload, "if"), "always()")
        self.assertIn("target/validation/cluster-topology.log", topology_upload)
        self.assertIn(
            "target/validation/cluster-topology-receipt.json", topology_upload
        )
        topology_propagate = topology["Propagate the topology contract result"]
        self.assertEqual(
            workflow_step_scalar(topology_propagate, "if"),
            "always() && steps.topology_contracts.outcome != 'success'",
        )
        self.assertEqual(
            workflow_step_scalar(topology_propagate, "run"), "exit 1"
        )

        self.assertIn(
            "cluster-check: cluster-wal-check cluster-store-check "
            "cluster-harness-check cluster-daemon-check",
            makefile,
        )

    def test_store_shards_are_dynamic_disjoint_and_roll_out_behind_one_verdict(self):
        workflow = read(".github/workflows/ci.yml")
        shards = read(".github/workflows/store-shards.yml")
        jobs = workflow_job_blocks(".github/workflows/ci.yml")
        shard_jobs = workflow_job_blocks(".github/workflows/store-shards.yml")
        legacy = jobs["cluster_store_legacy"]
        required = jobs["cluster_store_shards"]
        shadow = jobs["cluster_store_shadow"]
        shard = shard_jobs["shard"]
        aggregate = shard_jobs["aggregate"]
        verdict = jobs["cluster_store"]

        self.assertIn("execution_mode != 'accelerated'", legacy)
        self.assertIn("execution_mode == 'accelerated'", required)
        self.assertIn("uses: ./.github/workflows/store-shards.yml", required)
        self.assertIn("execution-mode: accelerated", required)
        self.assertIn("execution_mode == 'shadow'", shadow)
        self.assertIn("uses: ./.github/workflows/store-shards.yml", shadow)
        self.assertIn("execution-mode: shadow", shadow)
        self.assertIn(
            "continue-on-error: ${{ inputs.execution-mode == 'shadow' }}",
            shard,
        )

        # The sharded path has to be provable before anyone flips the
        # repository variable, and proving it must not cost pull-request
        # traffic. `workflow_dispatch` runs the same jobs on demand and
        # defaults to `shadow`, so a rehearsal is advisory unless it is asked
        # to be otherwise. Both triggers declare `execution-mode`, so every
        # expression reads the one resolved `inputs` value; `github.event.
        # inputs` is dispatch-only and would be empty on every `workflow_call`.
        call, dispatch = (
            shards.split("  workflow_call:\n", 1)[1].split("  workflow_dispatch:\n", 1)
        )
        self.assertIn("      execution-mode:\n        description:", call)
        self.assertIn("        required: true", call)
        self.assertIn("        type: string", call)
        self.assertIn("      execution-mode:\n        description:", dispatch)
        self.assertIn("        default: shadow", dispatch)
        self.assertIn("        type: choice", dispatch)
        self.assertNotIn(
            "github.event.inputs", shards.split("\njobs:\n", 1)[1]
        )
        # One shard per `ci-store` slot, every one selecting the same label set
        # that already schedules the legacy lane and the weekly backstop. A
        # shard with a runner class of its own is what produced
        # `ci-store-shard-1`, a label the fleet has never carried and which
        # would have queued every pull request forever the moment this path
        # became required.
        self.assertEqual(
            [("self-hosted", "Linux", "X64", "lab", "ci-store")],
            self_hosted_label_sets(shard),
        )
        self.assertIn("fail-fast: false", shard)
        # The count lives in one place and both invocations read it. The matrix
        # is the only unavoidable second mention — a matrix cannot be built
        # from `env` — so prove the two agree rather than pinning a literal
        # that every fleet change has to chase. A disagreement that somehow got
        # past this still fails closed, because `validation/store_shard.py`
        # rejects a receipt set whose size or index set does not match the
        # count the receipts declare.
        shard_count = store_shard_count()
        self.assertGreaterEqual(
            shard_count,
            2,
            "validation/store_shard.py rejects a shard count below two",
        )
        self.assertEqual(
            list(range(shard_count)),
            [
                int(index)
                for index in re.search(
                    r"(?m)^        shard_index: \[(.+)\]$", shard
                )
                .group(1)
                .split(",")
            ],
        )
        self.assertEqual(
            shard.count('--shard-count "$SHARD_COUNT"'),
            2,
            "both store_shard invocations must read the one declared count",
        )
        # No step may carry a literal count beside the one `env` declaration.
        self.assertNotRegex(shard, r"--shard-count \d")

        build = workflow_step_blocks(shard)["Build the exact Store test binary"]
        self.assertEqual(workflow_step_scalar(build, "continue-on-error"), "true")
        self.assertIn("uses: https://github.com/docker/build-push-action@v6", build)
        self.assertIn("file: Dockerfile.store-shard", build)
        self.assertIn("target: store-contract-binary", build)
        self.assertIn("platforms: linux/amd64", build)
        self.assertIn("outputs: type=local,dest=store-contract-export", build)
        self.assertIn("type=gha,scope=cluster-store-shard-{0}", build)
        self.assertIn("type=gha,mode=max,scope=cluster-store-shard-{0}", build)

        shard_steps = workflow_step_blocks(shard)
        run = shard_steps["Run the assigned Store tests once"]
        self.assertEqual(workflow_step_scalar(run, "continue-on-error"), "true")
        self.assertIn("python3 -m validation.store_shard run", run)
        self.assertIn('--shard-count "$SHARD_COUNT"', run)
        self.assertIn("--shard-index ${{ matrix.shard_index }}", run)
        self.assertNotIn("cargo test", run)
        failure = shard_steps["Record a Store binary build failure"]
        self.assertIn(
            "if: always() && steps.build_store_binary.outcome != 'success'",
            failure,
        )
        self.assertIn("python3 -m validation.store_shard record-failure", failure)
        upload = shard_steps["Retain the Store shard log and receipt"]
        self.assertEqual(workflow_step_scalar(upload, "if"), "always()")
        propagate = shard_steps["Propagate the Store shard result"]
        self.assertIn("steps.build_store_binary.outcome != 'success'", propagate)
        self.assertIn("steps.store_shard.outcome != 'success'", propagate)

        self.assertIn("needs: shard", aggregate)
        self.assertIn("python3 -m validation.store_shard validate", aggregate)
        self.assertIn("mkdir -p target/validation", aggregate)
        self.assertIn("pattern: cluster-store-shard-*", aggregate)
        self.assertIn("merge-multiple: true", aggregate)
        self.assertIn("cluster-store-shard-aggregate.json", aggregate)
        self.assertIn("needs.shard.result != 'success'", aggregate)
        self.assertIn("steps.validate_store_shards.outcome != 'success'", aggregate)

        self.assertIn("name: replicated Store contracts", verdict)
        self.assertNotIn("make cluster-store-check", verdict)
        select = workflow_step_blocks(verdict)["Select the required Store graph"]
        for contract in (
            'test "$LEGACY_RESULT" = success',
            'test "$LEGACY_RESULT" = skipped',
            'skipped|success) ;;',
            'test "$SHARD_RESULT" = success',
            'echo "Store shadow evidence is intentionally outside this gate"',
        ):
            self.assertIn(contract, select)
        self.assertIn("needs.cluster_store_shards.result", verdict)
        self.assertNotIn("cluster_store_shadow", verdict)
        self.assertEqual(workflow.count("cluster_store_shadow"), 1)
        pr_gate = jobs["pr_gate"]
        self.assertIn("      - cluster_store\n", pr_gate)
        self.assertNotIn("      - cluster_store_legacy\n", pr_gate)
        self.assertNotIn("      - cluster_store_shards\n", pr_gate)
        self.assertNotIn("      - cluster_store_shadow\n", pr_gate)

        dockerfile = read("Dockerfile.store-shard")
        self.assertIn("FROM rust:1.97.1-bookworm", dockerfile)
        self.assertIn("WORKDIR /src", dockerfile)
        self.assertIn("id=plurx-cargo-registry,sharing=locked", dockerfile)
        self.assertIn(
            "id=plurx-target-store-contract-${TARGETARCH},sharing=locked",
            dockerfile,
        )
        self.assertIn("--test store_contract --no-run", dockerfile)
        self.assertIn('test "$#" -eq 1', dockerfile)
        self.assertIn("rustc -Vv > /rustc-vv.txt", dockerfile)
        self.assertIn("FROM scratch AS store-contract-binary", dockerfile)

    def test_store_sharding_keeps_a_weekly_complete_unsharded_backstop(self):
        workflow = read(".github/workflows/cluster-store-backstop.yml")
        job = workflow_job_blocks(".github/workflows/cluster-store-backstop.yml")[
            "cluster-store-backstop"
        ]

        self.assertIn('cron: "23 6 * * 1"', workflow)
        self.assertIn("workflow_dispatch:", workflow)
        self.assertIn("runs-on: [self-hosted, Linux, X64, lab, ci-store]", job)
        self.assertIn("uses: https://github.com/dtolnay/rust-toolchain@1.97.1", job)
        self.assertIn("lane: cluster-store-backstop", job)
        self.assertIn('persistent-eligible: "true"', job)
        self.assertEqual(job.count("make cluster-store-check"), 2)
        self.assertNotIn("validation.store_shard run", job)
        self.assertNotIn("--exact", job)
        self.assertIn("cluster-store-backstop-receipt.json", job)
        self.assertIn(
            "if: always() && steps.store_backstop.outcome != 'success'", job
        )

    def test_required_arm_package_is_native_and_serialized_with_apple(self):
        workflow = read(".github/workflows/ci.yml")
        jobs = workflow_job_blocks(".github/workflows/ci.yml")
        package = jobs["package_smoke"]
        apple = jobs["apple"]
        pr_gate = jobs["pr_gate"]
        matrix = package.split("    steps:\n", 1)[0].split("include:\n", 1)[1]
        amd64 = matrix.split("- arch: amd64", 1)[1].split("- arch: arm64", 1)[0]
        arm64 = matrix.split("- arch: arm64", 1)[1]

        self.assertIn(
            "runs-on: ${{ fromJSON(matrix.runs_on) }}",
            package,
        )
        self.assertIn(
            "runs_on: "
            "'[\"self-hosted\",\"Linux\",\"ARM64\",\"lab\",\"ci-arm64\"]'",
            package,
        )
        self.assertIn("target: x86_64-unknown-linux-gnu", amd64)
        self.assertIn("kernel_machine: x86_64", amd64)
        self.assertIn("docker_machine: x86_64", amd64)
        self.assertIn(
            "runs_on: "
            "'[\"self-hosted\",\"Linux\",\"X64\",\"lab\",\"general\",\"high-cpu\"]'",
            amd64,
        )
        self.assertIn("target: aarch64-unknown-linux-gnu", arm64)
        self.assertIn("kernel_machine: aarch64", arm64)
        self.assertIn("docker_machine: aarch64", arm64)
        self.assertIn(
            "runs_on: "
            "'[\"self-hosted\",\"Linux\",\"ARM64\",\"lab\",\"ci-arm64\"]'",
            arm64,
        )
        self.assertNotIn("docker/setup-qemu-action", package)
        proof = workflow_step_blocks(package)[
            "Prove the package runner and Docker engine are native"
        ]
        self.assertIn('test "$(uname -s)/$(uname -m)"', proof)
        self.assertIn("Linux/$KERNEL_ARCH", proof)
        self.assertIn("docker info --format '{{.OSType}}/{{.Architecture}}'", proof)
        self.assertIn("DOCKER_ARCH: ${{ matrix.docker_machine }}", package)
        self.assertIn("docker_machine: aarch64", package)
        self.assertIn("linux/$DOCKER_ARCH", proof)
        self.assertIn(
            "persistent-eligible: ${{ matrix.arch == 'arm64' && 'true' || 'false' }}",
            package,
        )
        self.assertIn("package_smoke", workflow_job_needs(pr_gate))
        self.assertNotIn("native_arm_shadow", jobs)
        self.assertIn("plurx-apple-silicon-heavy", package)
        self.assertIn("plurx-apple-silicon-heavy", apple)
        self.assertIn("cancel-in-progress: false", package)
        self.assertIn("cancel-in-progress: false", apple)
        # Forgejo supports the standard concurrency group and cancellation
        # policy, but rejects the non-standard `queue` mapping key during
        # workflow schema validation before a runner can start.
        self.assertNotIn("queue:", package)
        self.assertNotIn("queue:", apple)

    def test_ci_caches_are_keyed_to_what_they_cache(self):
        workflow = read(".github/workflows/ci.yml")
        action = read(".github/actions/playwright/action.yml")
        jobs = workflow_job_blocks(".github/workflows/ci.yml")

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
        self.assertIn("PLURX_PLAYBACK_CHROME=$chromium", action)
        self.assertIn('test -x "$chromium"', action)
        self.assertNotIn("exit 0", action)

        # Both Android jobs reuse the Forgejo toolchain image keyed on the
        # Dockerfile hash, and the Makefile honors the pre-pull instead of
        # rebuilding the SDK image from scratch.
        self.assertEqual(
            workflow.count("sha256sum clients/android/Dockerfile"), 2
        )
        self.assertEqual(
            sum(
                jobs[name].count("secrets.LOCAL_REGISTRY_TOKEN")
                for name in ("android_jvm", "android_device")
            ),
            2,
        )
        self.assertEqual(
            jobs["publish_main"].count("secrets.LOCAL_REGISTRY_TOKEN"), 1
        )
        self.assertIn("192.168.4.7:3000/noirr/android-build", workflow)
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
            android_device.count("uses: https://github.com/reactivecircus/android-emulator-runner@v2"),
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
        store = jobs["cluster_store_legacy"]
        topology = jobs["cluster_topology"]
        wal = workflow.split("  cluster_wal:", 1)[1].split(
            "\n  cluster_daemon:", 1
        )[0]
        daemon = workflow.split("  cluster_daemon:", 1)[1].split(
            "\n  web_layout:", 1
        )[0]
        self.assertIn("uses: ./.github/actions/cargo-cache", store)
        self.assertIn("lane: cluster-store-legacy", store)
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
        label_template = (
            "docker image inspect --format "
            "'{{ index .Config.Labels \"org.opencontainers.image.revision\" }}'"
        )
        self.assertEqual(workflow.count(label_template), 1)
        self.assertNotIn(r'index .Config.Labels \"', workflow)
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
            workflow.count("uses: https://data.forgejo.org/forgejo/upload-artifact@v4"),
            workflow.count("retention-days:"),
        )
        self.assertIn("retention-days: 14", workflow)

        # GitHub's v4 artifact clients reject every non-GitHub server. Forgejo
        # publishes patched v4 clients with that host check removed; all local
        # workflows must use those clients for both upload and download.
        for workflow_path in (ROOT / ".github" / "workflows").glob("*.yml"):
            workflow_text = workflow_path.read_text(encoding="utf-8")
            self.assertNotIn(
                "https://data.forgejo.org/actions/upload-artifact@v4",
                workflow_text,
                workflow_path.name,
            )
            self.assertNotIn(
                "https://data.forgejo.org/actions/download-artifact@v4",
                workflow_text,
                workflow_path.name,
            )

        # Ordinary PRs retain only the small identity/digest receipt. Pushes
        # and final qualifications retain exact binaries for one day.
        build = workflow_job_blocks(".github/workflows/ci.yml")["package_smoke"]
        self.assertIn(
            "name: Retain candidate binaries for push, tag, and qualification runs",
            build,
        )
        self.assertIn(
            "name: Retain candidate binaries for non-qualification push and tag runs",
            build,
        )
        self.assertIn("needs.scope.outputs.qualification == 'true'", build)
        self.assertIn("name: Retain the exact package receipt", build)
        self.assertIn("release-bin/*.sha256", build)
        blocking_upload = workflow_step_blocks(build)[
            "Retain candidate binaries for push, tag, and qualification runs"
        ]
        advisory_upload = workflow_step_blocks(build)[
            "Retain candidate binaries for non-qualification push and tag runs"
        ]
        self.assertNotIn("continue-on-error:", blocking_upload)
        self.assertEqual(
            workflow_step_scalar(advisory_upload, "continue-on-error"), "true"
        )
        self.assertIn("github.event_name == 'push'", advisory_upload)
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
        self.assertIn("REGISTRY_IMAGE: 192.168.4.7:3000/noirr/plurxd", publisher)
        self.assertEqual(publisher.count("secrets.LOCAL_REGISTRY_TOKEN"), 4)
        self.assertEqual(
            publisher.count(
                "buildkitd-config: ${{ github.workspace }}/.github/buildkitd.toml"
            ),
            4,
        )
        self.assertIn(
            "<Repository>192.168.4.7:3000/noirr/plurxd:main</Repository>",
            unraid,
        )
        self.assertIn(
            "<Registry>http://192.168.4.7:3000/noirr/-/packages/container/plurxd/main</Registry>",
            unraid,
        )
        self.assertIn('cron: "41 16 * * 1"', readiness)
        self.assertIn("run: make release-check", readiness)
        self.assertIn("fetch-depth: 0", readiness)

    def test_main_push_builds_once_and_publishes_only_after_validation(self):
        workflow = read(".github/workflows/ci.yml")
        jobs = workflow_job_blocks(".github/workflows/ci.yml")
        publish = jobs["publish_main"]
        script = read("scripts/registry-push")

        self.assertIn("name: publish merged image (Forgejo registry)", publish)
        self.assertIn("github.event_name == 'push'", publish)
        self.assertIn("github.ref == 'refs/heads/main'", publish)
        for dependency in (
            "check",
            "cluster_store",
            "cluster_topology",
            "cluster_wal",
            "cluster_daemon",
            "web_layout",
            "vod_web",
            "package_smoke",
        ):
            self.assertIn(f"      - {dependency}\n", publish)
        self.assertIn("Require the post-merge validation fan-out", publish)
        self.assertIn("secrets.LOCAL_REGISTRY_TOKEN", publish)
        self.assertIn("run: scripts/registry-push", publish)
        self.assertIn("PLURX_BUILD_SHA: ${{ github.sha }}", publish)

        self.assertIn('IMMUTABLE_REF="$IMAGE:sha-$SHORT_SHA"', script)
        self.assertIn('FLEET_REF="$IMAGE:main"', script)
        self.assertNotIn('plurxd:latest', script)
        self.assertLess(
            script.index('verify_image "$IMMUTABLE_REF"'),
            script.index('docker push "$FLEET_REF"'),
        )
        self.assertIn('test "$fleet_digest" = "$immutable_digest"', script)
        self.assertIn("refusing to publish a checkout with tracked changes", script)
        self.assertIn("registry state is indeterminate; refusing to publish", script)

    def test_every_actions_job_has_an_explicit_timeout(self):
        for path in (
            ".github/workflows/cluster-store-backstop.yml",
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

    def test_forgejo_workflows_do_not_declare_ignored_github_permissions(self):
        workflow_dir = ROOT / ".github/workflows"
        paths = sorted((*workflow_dir.glob("*.yml"), *workflow_dir.glob("*.yaml")))
        for path in paths:
            with self.subTest(path=path.relative_to(ROOT)):
                workflow = path.read_text(encoding="utf-8")
                self.assertNotRegex(workflow, r"(?m)^\s*permissions:\s*$")

    def test_rust_audit_can_report_informational_advisories(self):
        workflow = read(".github/workflows/rust-audit.yml")
        jobs = workflow_job_blocks(".github/workflows/rust-audit.yml")

        for name in ("workspace", "fuzz"):
            with self.subTest(name=name):
                self.assertIn("if: github.event_name != 'schedule'", jobs[name])
        scheduled = jobs["scheduled"]
        self.assertIn("if: github.event_name == 'schedule'", scheduled)
        self.assertEqual(scheduled.count("uses: https://github.com/rustsec/audit-check@"), 1)
        self.assertIn("--additional-lock fuzz/Cargo.lock", scheduled)
        self.assertIn("working-directory: target/rust-audit", scheduled)
        self.assertEqual(workflow.count("token: ${{ secrets.FORGEJO_TOKEN }}"), 3)

    def test_ci_jobs_use_the_intended_runner_trust_boundary(self):
        def local(*labels):
            return f"    runs-on: [{', '.join(('self-hosted', *labels))}]"

        general = local("Linux", "X64", "lab", "general")
        high_cpu = local("Linux", "X64", "lab", "general", "high-cpu")
        ffmpeg6 = local("Linux", "X64", "lab", "general", "ffmpeg-6")
        high_cpu_ffmpeg6 = local(
            "Linux", "X64", "lab", "general", "high-cpu", "ffmpeg-6"
        )
        android = local("Linux", "X64", "lab", "android-kvm")
        apple = local("macOS", "ARM64", "lab", "apple", "xcode-26")
        ci_store = local("Linux", "X64", "lab", "ci-store")
        ci_topology = local("Linux", "X64", "lab", "ci-topology")
        release_general = "    runs-on: [self-hosted, Linux, X64, lab, general]"
        release_high_cpu = (
            "    runs-on: [self-hosted, Linux, X64, lab, general, high-cpu]"
        )

        for path in (
            ".github/workflows/ci.yml",
            ".github/workflows/cluster-store-backstop.yml",
            ".github/workflows/effort-ci.yml",
            ".github/workflows/fix-evidence.yml",
            ".github/workflows/lint.yml",
            ".github/workflows/release-readiness.yml",
            ".github/workflows/rust-audit.yml",
            ".github/workflows/store-shards.yml",
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
                elif path == ".github/workflows/ci.yml" and name == "package_smoke":
                    expected = "    runs-on: ${{ fromJSON(matrix.runs_on) }}"
                elif path == ".github/workflows/ci.yml" and name == "publish_main":
                    expected = high_cpu
                elif (
                    path == ".github/workflows/ci.yml"
                    and name == "cluster_store_legacy"
                ):
                    expected = ci_store
                elif (
                    path == ".github/workflows/store-shards.yml"
                    and name == "shard"
                ):
                    # Every shard selects the one label set, inline, exactly as
                    # the legacy lane and the weekly backstop do. Reading it
                    # from a per-shard matrix key is what let one row drift to
                    # a label the fleet does not carry.
                    expected = ci_store
                elif (
                    path == ".github/workflows/cluster-store-backstop.yml"
                    and name == "cluster-store-backstop"
                ):
                    expected = ci_store
                elif path == ".github/workflows/ci.yml" and name == "cluster_topology":
                    expected = ci_topology
                elif path == ".github/workflows/ci.yml" and name == "check":
                    expected = high_cpu_ffmpeg6
                elif path == ".github/workflows/ci.yml" and name == "cluster_wal":
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
                    expected = general
                elif (
                    "uses: ./.github/actions/ffmpeg" in block
                    and "container: ubuntu:26.04" not in block
                ):
                    expected = ffmpeg6
                self.assertEqual(expected, runs_on.group(0), f"{path}:{name}")

        for path in (ROOT / ".github/workflows").glob("*.yml"):
            self.assertNotIn("CI_RUNNER_MODE", path.read_text(encoding="utf-8"))

        for path in (".github/workflows/publish-release.yml",):
            for name, block in workflow_job_blocks(path).items():
                runs_on = re.search(r"(?m)^    runs-on: .+$", block)
                self.assertIsNotNone(runs_on, f"{path}:{name} has no runner")
                expected = release_general if name == "resolve" else release_high_cpu
                self.assertEqual(expected, runs_on.group(0), f"{path}:{name}")

        for name, block in workflow_job_blocks(
            ".github/workflows/rust-audit.yml"
        ).items():
            self.assertIn(
                "uses: https://github.com/dtolnay/rust-toolchain@1.97.1",
                block,
                f"rust-audit:{name} does not provision the pinned Cargo toolchain",
            )

    def test_every_self_hosted_runs_on_is_satisfiable_by_a_rostered_runner(self):
        # A `runs-on` naming a label nothing carries is not an error GitHub
        # reports. The job queues, indefinitely, and is eventually cancelled
        # having never started — which is how the required Store lane spent
        # 2026-09-02 at 45 attempts, 15 started, 27 cancelled while queued, and
        # waits reaching 234 minutes, and how `store-shards.yml` came to pin a
        # shard to `ci-store-shard-1`, a label the fleet has never had. Nothing
        # in the repository knew what the fleet carries, so nothing could
        # object. `validation/runner-fleet.toml` is that knowledge and this is
        # the check that spends it, at preflight, in milliseconds.
        #
        # Discover the workflow files rather than listing them: a workflow
        # added after this test must not be exempt from it merely by being
        # newer than it.
        runners = rostered_runners()
        self.assertTrue(runners, "the runner roster is empty")

        workflow_paths = sorted(
            path.relative_to(ROOT).as_posix()
            for pattern in ("*.yml", "*.yaml")
            for path in (ROOT / ".github/workflows").glob(pattern)
        )
        self.assertIn(".github/workflows/store-shards.yml", workflow_paths)

        selections = 0
        for path in workflow_paths:
            parsed: list[tuple[str, ...]] = []
            for name, block in workflow_job_blocks(path).items():
                for labels in self_hosted_label_sets(block):
                    parsed.append(labels)
                    selections += 1
                    with self.subTest(path=path, job=name, labels=labels):
                        self.assertTrue(
                            [
                                runner
                                for runner, carried in runners
                                if set(labels) <= carried
                            ],
                            f"{path}:{name} selects {list(labels)}, which no "
                            "runner in validation/runner-fleet.toml carries. "
                            "Give a runner the label before a workflow asks "
                            "for it.",
                        )
            # The per-job parse above has to see every self-hosted selection in
            # the file. A `runs-on` spelling it cannot read would otherwise be
            # quietly exempt, which is the same "nothing was looking" failure
            # wearing a different hat, so compare against the whole file.
            self.assertEqual(
                sorted(parsed),
                sorted(self_hosted_label_sets(read(path))),
                f"{path} selects a self-hosted runner in a shape this test "
                "cannot parse; teach self_hosted_label_sets to read it",
            )
            for declaration in re.findall(r"(?m)^ +runs-on: (.+)$", read(path)):
                self.assertTrue(
                    declaration.startswith("[self-hosted, ")
                    or declaration == "${{ fromJSON(matrix.runs_on) }}",
                    f"{path} has a non-local runner declaration: {declaration}",
                )
        # Guard against the parser silently matching nothing at all and the
        # loop above passing vacuously.
        self.assertGreater(selections, 20)

    def test_the_runner_roster_keeps_heavy_cluster_lanes_off_shared_hosts(self):
        # The roster is only worth what its own shape guarantees. `ci-store`
        # and `ci-topology` each select a three-voter hiqlite test; two of them
        # on one physical host is the loaded-runner quorum flake that produced
        # the artwork-fence failures, and one on a production voter is the same
        # bet with plurx's own availability as the stake. Both invariants are
        # deliberate, so both are written down here rather than remembered.
        fleet = runner_fleet()
        runners = fleet["runners"]
        voters = set(fleet["voter_hosts"])
        self.assertTrue(voters)

        names = [runner["name"] for runner in runners]
        self.assertEqual(len(names), len(set(names)), "duplicate runner name")
        for runner in runners:
            with self.subTest(runner=runner["name"]):
                # `gha-<host>-<role>-<NN>`. The host is in the name, so a
                # runner cannot be filed under the wrong machine and quietly
                # buy a second slot for its host.
                self.assertEqual(runner["name"].split("-")[1], runner["host"])
                self.assertIn(runner["os"], {"Linux", "macOS"})
                self.assertIn(runner["arch"], {"X64", "ARM64"})
                for implicit in ("self-hosted", runner["os"], runner["arch"]):
                    self.assertNotIn(
                        implicit,
                        runner["labels"],
                        "labels lists what the fleet assigns; GitHub adds this "
                        "one and ansible never writes it",
                    )

        for label in fleet["host_exclusive_labels"]:
            hosts = [
                runner["host"] for runner in runners if label in runner["labels"]
            ]
            with self.subTest(label=label):
                self.assertTrue(hosts, f"no rostered runner carries {label}")
                self.assertEqual(
                    sorted(hosts),
                    sorted(set(hosts)),
                    f"two {label} runners share a physical host",
                )
                self.assertEqual(
                    [],
                    sorted(set(hosts) & voters),
                    f"{label} is assigned to a production plurx voter",
                )

        # The Store fan-out is only as parallel as the fleet. One host holds at
        # most one slot, so a surplus shard cannot be given a runner of its
        # own: it serialises behind a busy one and buys a more complicated
        # failure and no wall time. This is the check that caught a three-shard
        # matrix on 2026-09-02 after `gha-nuc2-android-01` lost `ci-store` for
        # failing the lane on an unwritable Cargo home.
        self.assertGreaterEqual(
            len([r for r in runners if "ci-store" in r["labels"]]),
            store_shard_count(),
            "the Store shard count exceeds the number of ci-store slots",
        )

    def test_root_job_containers_restore_persistent_workspace_ownership(self):
        workflow_root = ROOT / ".github/workflows"
        for path in sorted(workflow_root.glob("*.y*ml")):
            relative = path.relative_to(ROOT).as_posix()
            for name, block in workflow_job_blocks(relative).items():
                if "\n    container:" not in block:
                    continue
                runs_on = re.search(r"(?m)^    runs-on: .+$", block)
                self.assertIsNotNone(runs_on, f"{relative}:{name} has no runner")
                self.assertIn("self-hosted", runs_on.group(0))
                self.assertIn(
                    "Restore persistent runner workspace ownership",
                    block,
                    f"{relative}:{name} does not repair root container output",
                )

    def test_workflows_have_no_hosted_runner_escape(self):
        script = ROOT / "scripts/ci-runner-mode"

        self.assertFalse(script.exists())
        for path in (ROOT / ".github/workflows").glob("*.yml"):
            workflow = path.read_text(encoding="utf-8")
            self.assertNotIn("CI_RUNNER_MODE", workflow)
            self.assertNotRegex(workflow, r"(?m)^ +runs-on: (?:ubuntu|macos)-")

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
        reporter = script.read_text(encoding="utf-8")
        self.assertIn('default="http://192.168.4.7:3000/api/v1"', reporter)
        self.assertIn('default="noirr/plurx"', reporter)
        self.assertIn('os.environ.get("FORGEJO_TOKEN")', reporter)
        self.assertNotIn("api.github.com", reporter)
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
        self.assertNotRegex(smoke, r"curl [^\n]+\| grep -q")
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
