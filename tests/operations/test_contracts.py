from __future__ import annotations

import json
import os
from pathlib import Path
import re
import runpy
import subprocess
import sys
import tempfile
import textwrap
import tomllib
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[2]

# The fleet registry reaches every workflow through one repository variable,
# and every use carries the same fallback so an unset variable can never
# resolve to Docker Hub (an empty registry means docker.io to `docker login`
# and to `docker/login-action`).
FLEET_REGISTRY_EXPR = "${{ vars.FLEET_REGISTRY || 'fleet-registry.unset.invalid' }}"
# Workflows that run only on the public GitHub mirror, on GitHub-hosted
# runners. Every other workflow is Forgejo CI on the self-hosted fleet and is
# held to that boundary below; these are held to their own contract instead
# (test_github_only_workflows_never_reach_the_fleet), which is what makes the
# exemption safe rather than a hole.
GITHUB_ONLY_WORKFLOWS = frozenset({
    ".github/workflows/github-release.yml",
    ".github/workflows/desktop-qualification.yml",
})
GITHUB_HOSTED_RUNNERS = frozenset(
    {"ubuntu-24.04", "ubuntu-24.04-arm", "macos-15", "macos-15-intel", "windows-2022"}
)


def read(path: str) -> str:
    return (ROOT / path).read_text(encoding="utf-8")


def make_dry_run_commands(target: str, *variables: str) -> list[str]:
    result = subprocess.run(
        ["make", "--no-print-directory", "-n", target, *variables],
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
    return values[0].split(" #", 1)[0].rstrip()


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
    def test_public_oracle_fetch_keeps_deadline_checksum_and_size_bounds(self):
        import hashlib
        provision = runpy.run_path(str(ROOT / "scripts/provision-hiqlite-upstream"))
        payload = b"a pinned public archive fixture"
        provision["CRATES"]["hiqlite"] = hashlib.sha256(payload).hexdigest()

        def fetched(command, **options):
            if command == ["curl", "-q", "--version"]:
                return subprocess.CompletedProcess(command, 0, "curl 8.4.0 test fixture")
            self.assertEqual(command[:2], ["curl", "-q"])
            self.assertEqual(options, {"check": True, "timeout": 25})
            self.assertNotIn("--insecure", command)
            for flag, value in (("--max-time", "20"), ("--connect-timeout", "5"),
                                ("--max-filesize", str(provision["ARCHIVE_LIMIT"])),
                                ("--proto", "=https"), ("--proto-redir", "=https")):
                self.assertEqual(command[command.index(flag) + 1], value)
            self.assertEqual(command[-1],
                             "https://static.crates.io/crates/hiqlite/hiqlite-0.14.0.crate")
            Path(command[command.index("--output") + 1]).write_bytes(payload)

        with mock.patch("subprocess.run", side_effect=fetched):
            self.assertEqual(provision["archive_bytes"]("hiqlite", None), payload)
        payload = b"substituted archive"
        with mock.patch("subprocess.run", side_effect=fetched):
            with self.assertRaisesRegex(ValueError, "registry checksum mismatch"):
                provision["archive_bytes"]("hiqlite", None)
        payload = b"x" * (provision["ARCHIVE_LIMIT"] + 1)
        with mock.patch("subprocess.run", side_effect=fetched):
            with self.assertRaisesRegex(ValueError, "exceeds its bound"):
                provision["archive_bytes"]("hiqlite", None)

    def test_public_oracle_refuses_curl_without_unknown_length_size_enforcement(self):
        provision = runpy.run_path(str(ROOT / "scripts/provision-hiqlite-upstream"))
        for version in ("curl 8.3.0 test fixture", "unrecognized curl"):
            with self.subTest(version=version), mock.patch(
                    "subprocess.run", return_value=subprocess.CompletedProcess([], 0, version)) as run:
                with self.assertRaisesRegex(ValueError, "curl >= 8.4.0"):
                    provision["archive_bytes"]("hiqlite", None)
                self.assertEqual(run.call_count, 1)
                self.assertEqual(run.call_args.args[0], ["curl", "-q", "--version"])

    def test_public_oracle_download_failure_cannot_publish_a_fixture(self):
        provision = runpy.run_path(str(ROOT / "scripts/provision-hiqlite-upstream"))
        with tempfile.TemporaryDirectory() as temporary:
            destination = Path(temporary) / "fixture"
            with mock.patch.object(sys, "argv", ["provision", str(destination)]), \
                    mock.patch("subprocess.run", side_effect=[
                        subprocess.CompletedProcess([], 0, "curl 8.4.0 test fixture"),
                        subprocess.TimeoutExpired("curl", 25),
                    ]):
                with self.assertRaises(subprocess.TimeoutExpired):
                    provision["main"]()
            self.assertFalse(destination.exists())
            self.assertEqual(list(Path(temporary).iterdir()), [])

    def test_decoder_artifact_hash_opens_once_and_enforces_the_stream_ceiling(self):
        source = read("crates/plurxd/src/decoder_health.rs")
        digest = source.split("async fn file_digest", 1)[1].split(
            "async fn run", 1
        )[0]

        self.assertLess(digest.index("File::open(path)"), digest.index("file.metadata()"))
        self.assertIn("total.checked_add(read as u64)?", digest)
        self.assertIn("total > MAX_ARTIFACT_BYTES", digest)

    def test_external_actions_are_immutable_and_checkout_drops_credentials(self):
        action_paths = sorted((ROOT / ".github/actions").glob("*/action.yml"))
        workflow_paths = sorted((ROOT / ".github/workflows").glob("*.yml"))
        checkout_count = 0
        credential_drop_count = 0
        for path in (*action_paths, *workflow_paths):
            source = path.read_text(encoding="utf-8")
            for match in re.finditer(
                r"(?m)^\s*(?:-\s+)?uses:\s+(https://\S+)", source
            ):
                action = match.group(1)
                self.assertRegex(
                    action,
                    r"@[0-9a-f]{40}$",
                    f"{path.relative_to(ROOT)} has a mutable action input: {action}",
                )
            checkout_count += source.count(
                "https://data.forgejo.org/actions/checkout@"
            )
            credential_drop_count += source.count("persist-credentials: false")
        self.assertEqual(checkout_count, credential_drop_count)

    def test_live_tv_ffmpeg_clears_inherited_environment(self):
        live_tv = read("crates/plurxd/src/live_tv.rs")
        command = live_tv.split("fn live_ffmpeg_command(", 1)[1].split(
            "fn live_probe_args", 1
        )[0]
        self.assertIn(".env_clear()", command)
        self.assertIn('.env("LC_ALL", "C")', command)

    def test_live_tv_browser_fixture_traverses_master_and_recovers_exact_request(self):
        import ast
        from types import SimpleNamespace
        from urllib.parse import urlsplit

        tree = ast.parse(read("scripts/live-tv-browser"))
        intercept = next(node for node in ast.walk(tree)
                         if isinstance(node, ast.FunctionDef) and node.name == "intercept")
        with tempfile.TemporaryDirectory() as temporary:
            media = Path(temporary)
            (media / "0.ts").write_bytes(b"synthetic transport stream")
            sessions = {"fixture-1": {"opened": 0, "closed": False}}
            violations = []
            namespace = {"re": re, "urlsplit": urlsplit, "media": media,
                         "time": SimpleNamespace(monotonic=lambda: 0),
                         "sessions": sessions, "violations": violations,
                         "host": SimpleNamespace(token="fixture-token"),
                         "channels": [{"id": "one"}], "events": [],
                         "mode": {"next": "normal"}, "starts_by_id": {},
                         "start_requests": [], "resumes": [], "retirements": []}
            namespace["active"] = lambda: [key for key, value in sessions.items() if not value["closed"]]
            exec(compile(ast.Module(body=[intercept], type_ignores=[]),
                         "scripts/live-tv-browser", "exec"), namespace)

            def request(path, method="GET", headers=None, body=None):
                response = {}
                incoming = SimpleNamespace(
                    url="http://fixture.invalid/api/v1/live-tv/" + path,
                    method=method, headers=headers or {}, post_data_json=body)
                namespace["intercept"](SimpleNamespace(
                    request=incoming, fulfill=lambda **value: response.update(value)))
                return response

            def get(suffix, headers=None):
                return request("sessions/fixture-1/" + suffix, headers=headers)

            master = get("master.m3u8")
            self.assertEqual(master["content_type"], "application/vnd.apple.mpegurl")
            variants = [line for line in master["body"].splitlines()
                        if line and not line.startswith("#")]
            self.assertEqual(variants, ["index.m3u8"])
            self.assertIn("#EXT-X-STREAM-INF:", master["body"])
            playlist = get(variants[0])
            segments = [line for line in playlist["body"].splitlines()
                        if line and not line.startswith("#")]
            self.assertEqual(segments[0], "0.ts")
            self.assertEqual(get(segments[0])["body"], b"synthetic transport stream")
            self.assertEqual(violations, [])
            with self.assertRaises(AssertionError):
                get("unexpected.m3u8")
            get("master.m3u8", {"authorization": "Bearer fixture-only"})
            self.assertEqual(len(violations), 1, "capability reads still flag account credentials")
            sessions["fixture-1"]["closed"] = True
            self.assertEqual(get("master.m3u8")["status"], 410)

            auth = {"authorization": "Bearer fixture-token"}
            request_id = "a" * 32
            path = "starts/" + request_id
            activation = request("channels/one/sessions", "POST", auth,
                                 {"request_id": request_id})["json"]
            self.assertEqual(namespace["starts_by_id"][request_id], activation["session_id"])
            self.assertEqual(request("channels/one/sessions", "POST", auth,
                                     {"request_id": request_id})["json"], activation)
            self.assertEqual(len(sessions), 2, "replay must not allocate another capability")
            self.assertEqual(request(path + "/resume", "POST")["status"], 401)
            self.assertEqual(request(path, "DELETE")["status"], 401)
            self.assertFalse(sessions[activation["session_id"]]["closed"])
            self.assertEqual(request("starts/" + "b" * 32 + "/resume", "POST", auth)["status"], 404)
            resumed = request(path + "/resume", "POST", auth)["json"]
            self.assertEqual(resumed, {"outcome": "live", "session": activation})
            self.assertEqual(namespace["resumes"], [request_id])
            self.assertEqual(len(sessions), 2, "resume must not tune")
            self.assertEqual(request(path, "DELETE", auth)["json"], {"outcome": "retired"})
            self.assertEqual(request(path + "/resume", "POST", auth)["json"], {"outcome": "ended"})
            self.assertEqual(request(path, "DELETE", auth)["json"], {"outcome": "retired"})
            self.assertEqual(namespace["events"], ["start:fixture-2", "release:fixture-2"])
            self.assertEqual(request("channels/one/sessions", "POST", auth,
                                     {"request_id": ""})["status"], 400)
            self.assertEqual(len(sessions), 2)

    def test_private_origin_fetches_use_only_step_scoped_credentials(self):
        checked = 0
        for path in sorted((ROOT / ".github/workflows").glob("*.yml")):
            workflow = path.read_text(encoding="utf-8")
            fetches = list(re.finditer(r"fetch --no-tags --force origin", workflow))
            checked += len(fetches)
            for fetch in fetches:
                step_start = workflow.rfind("\n      - name:", 0, fetch.start())
                step_end = workflow.find("\n      - ", fetch.end())
                step = workflow[step_start : None if step_end < 0 else step_end]
                self.assertIn(
                    "GITHUB_TOKEN: ${{ github.token }}",
                    step,
                    path.relative_to(ROOT),
                )
                self.assertIn(
                    'http.extraheader="AUTHORIZATION: basic $auth"',
                    step,
                    path.relative_to(ROOT),
                )
                self.assertNotIn("git config", step, path.relative_to(ROOT))
        self.assertGreaterEqual(checked, 4)

    def test_browser_validation_never_claims_host_audio_or_media_controls(self):
        for path in ("scripts/playback-lab", "scripts/ui-baseline"):
            with self.subTest(path=path):
                script = read(path)
                self.assertIn('"--mute-audio"', script)
                self.assertIn("HardwareMediaKeyHandling", script)

    def test_ui_baseline_waits_for_current_boot_caption_probe_and_registered_children(self):
        from types import SimpleNamespace
        from unittest.mock import patch

        harness = runpy.run_path(str(ROOT / "scripts/ui-baseline"), run_name="caption_fixture_contract")
        wait = harness["SelfHost"].wait_for_caption_probe
        source = read("scripts/ui-baseline")
        self.assertIn("probe_log_offset = self.log.stat().st_size\n        self.spawn()\n        self.wait_for_caption_probe(probe_log_offset)", source)
        self.assertIn("self.require_jellyfin_listener()", source)
        with tempfile.TemporaryDirectory() as temporary:
            log = Path(temporary) / "daemon.log"
            old = b"caption graph probe complete\n"
            log.write_bytes(old + b"new boot\n")
            fixture = SimpleNamespace(log=log, proc=SimpleNamespace(poll=lambda: None),
                                      base_url="http://fixture.invalid", token="fixture-only")
            def observe(read_processes, read_boot_log, alive, **clock):
                self.assertEqual(read_boot_log(), "new boot\n")
                self.assertTrue(alive())
                self.assertEqual(read_processes(3), [{"pid": 123}])
                self.assertIn("now", clock)
                self.assertIn("sleep", clock)
            with patch.dict(wait.__globals__, {"wait_for_fixture_idle": observe,
                    "api": lambda *args, **kwargs: {"processes": [{"pid": 123}]}}):
                wait(fixture, len(old))
            with patch.dict(wait.__globals__, {"wait_for_fixture_idle": observe,
                    "api": lambda *args, **kwargs: {}}):
                with self.assertRaises(harness["Fail"]):
                    wait(fixture, len(old))

    def test_ui_baseline_starts_poll_observation_after_route_settles(self):
        script = read("scripts/ui-baseline")

        # The contract is that every route publishing a settled phase is waited
        # on before the polling observation window opens — not that the set is
        # spelled on one line. Pinning the literal made adding a route a
        # two-file edit whose second file had nothing to do with the change.
        settled_gate = re.search(
            r"if name in \{([^}]*)\}:\s*\n\s*page\.wait_for_selector\(", script
        )
        self.assertIsNotNone(settled_gate, "ui-baseline no longer gates on the settled phase")
        waited = set(re.findall(r'"([^"]+)"', settled_gate.group(1)))
        for route in ("home", "activity", "analysis", "settings", "settings-developer",
                      "live-tv", "live-tv-grid"):
            self.assertIn(route, waited, f"{route} publishes a settled phase but is not waited on")
        self.assertIn('[data-phase="settled"]', script)
        self.assertIn('wait_until="domcontentloaded"', script)
        self.assertIn("activity_poll_paused = pause_activity_polling", script)
        self.assertIn('page.wait_for_load_state("networkidle"', script)
        self.assertIn("resume_activity_polling(page)", script)
        # The settings tick budget is enforced inside the page, not by counting
        # requests from Python. A count read over a round trip cannot bound a
        # 2s timer: on a starved runner it is handed the total only after the
        # interval has fired again, and the golden then fails on
        # `GET /api/v1/scan/status 3 -> 4` with nothing about the page changed.
        # So what this pins is the clamp and the boundary the capture waits on.
        self.assertIn("window.__plurxSettingsScanTicks >= 2", script)
        self.assertIn("window.__plurxSettingsScanCaptureTick = true", script)
        self.assertIn(
            '"() => window.__plurxSettingsScanCaptureTick === true"', script
        )
        self.assertNotIn(
            'api_calls.count("GET /api/v1/scan/status") <', script,
            "the settings poll budget must not go back to a Python-side count",
        )
        # Settings waits for the activity fetch to finish like every other
        # route. It used to be excluded, so the recorders could run while the
        # response was in flight and re-render the page under the tab walk.
        self.assertIn("if activity_poll_paused:", script)
        self.assertIn(
            "window.__plurxGlobalActivityCaptureTick === true && !ACT_POLLING",
            script,
        )
        self.assertNotIn('if activity_poll_paused and name != "settings":', script)
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
        pinned = script.index('"vod_index_mins": 0', seed)
        libraries = script.index("for name, kind, share in LIBRARIES:", seed)

        self.assertLess(pinned, libraries)

    def test_activity_capture_observes_one_real_tick_without_hiding_extra_requests(self):
        script = read("scripts/ui-baseline")
        capture = runpy.run_path(str(ROOT / "scripts/ui-baseline"))["ACTIVITY_CAPTURE_JS"]
        # setPageTimer and render are both the router's; the shell is markup
        # and tags now (docs/clients/WEB-SHELL-LAYOUT.md).
        web = read("crates/plurxd/src/web/router.js")
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

    def test_global_activity_capture_stops_before_runner_delay_adds_a_second_tick(self):
        script = read("scripts/ui-baseline")
        capture = runpy.run_path(str(ROOT / "scripts/ui-baseline"))["ACTIVITY_CAPTURE_JS"]
        contract = r"""
const assert = require('node:assert/strict');
const vm = require('node:vm');
const capture = process.argv[1];
let now = 0, next = 0;
const timers = new Map(), calls = [];
const context = vm.createContext({
  location: {hash: '#/item/7'}, ACT_TIMER: null,
  setInterval(callback, delay, ...args) {
    const id = ++next;
    timers.set(id, {callback, delay, args, due: now + delay});
    return id;
  },
  clearInterval(id) { timers.delete(id); },
  record: value => calls.push(value),
});
context.window = context;
vm.runInContext(capture, context);
vm.runInContext(`
  ACT_TIMER = setInterval(() => record('activity'), 4000);
  setInterval(() => record('unrelated'), 4000);
`, context);
const target = 8500; // Model a badly delayed 4.5s Playwright sleep callback.
while (true) {
  const ready = [...timers].filter(([, timer]) => timer.due <= target)
    .sort((a, b) => a[1].due - b[1].due)[0];
  if (!ready) break;
  const [id, timer] = ready;
  now = timer.due;
  timer.due += timer.delay;
  timer.callback(...timer.args);
  if (timers.has(id)) timers.set(id, timer);
}
assert.deepEqual(calls.filter(call => call === 'activity'), ['activity']);
assert.deepEqual(calls.filter(call => call === 'unrelated'), ['unrelated', 'unrelated']);
assert.equal(context.__plurxGlobalActivityCaptureTick, true);
assert.equal(context.ACT_TIMER, null);
"""
        subprocess.run(["node", "-e", contract, capture], check=True)
        self.assertIn("window.__plurxGlobalActivityCaptureTick = false;", script)
        self.assertIn("window.__plurxGlobalActivityCaptureTick === true && !ACT_POLLING", script)

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
        ports = contract["free_ports"](0, 4)

        self.assertEqual(len(ports), 4)
        self.assertEqual(len(set(ports)), 4)
        self.assertTrue(all(port > 0 for port in ports))
        self.assertIn("self.port, self.raft_port, self.api_port, self.jellyfin_port = free_ports", script)
        self.assertIn('jellyfin_port = {self.jellyfin_port}', script)
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
            'start_period: "${PLURX_HEALTH_START_PERIOD:-25m}"', compose
        )
        self.assertIn(
            'PLURX_CLUSTER_SNAPSHOT_CHUNK_TIMEOUT_SECS: '
            '"${PLURX_CLUSTER_SNAPSHOT_CHUNK_TIMEOUT_SECS:-}"',
            compose,
        )
        self.assertIn(
            'PLURX_CLUSTER_SNAPSHOT_TRANSFER_TIMEOUT_SECS: '
            '"${PLURX_CLUSTER_SNAPSHOT_TRANSFER_TIMEOUT_SECS:-}"',
            compose,
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
        self.assertEqual(
            compose.count("image: ${PLURX_IMAGE:-plurx/plurxd:latest}"),
            2,
        )
        self.assertNotRegex(server, r"(?m)^    network_mode: host$")
        self.assertIn("network_mode: host", discovery)
        configured_origin = '"http://127.0.0.1:${PLURX_HTTP_PORT:-32400}"'
        configured_bind = '"127.0.0.1:${PLURX_HTTP_PORT:-32400}"'
        self.assertIn(f"PLURX_DISCOVERY_SERVER_URL: {configured_origin}", discovery)
        self.assertIn(f"PLURX_BIND: {configured_bind}", discovery)

    def _run_rollout_recipe(
        self,
        target: str,
        up_command: str,
        proof_exit: int,
        *,
        hardware_exit: int = 0,
        image_revision: str = "a" * 40,
        checkout_revision: str = "a" * 40,
        tracked_dirty: bool = False,
        discovery_image: str | None = None,
    ) -> tuple[int, Path, Path, Path, Path, Path]:
        """Run a real rollout recipe with the checker and Docker stubbed."""

        rollout = [
            line
            for line in make_dry_run_commands(target)
            if up_command in line
        ][0]
        directory = Path(tempfile.mkdtemp())
        pull_marker = directory / "compose-pull-ran"
        derive_marker = directory / "budget-derive-ran"
        proof_marker = directory / "budget-proof-ran"
        up_marker = directory / "compose-up-ran"
        image_marker = directory / "compose-up-image"
        stubs = directory / "bin"
        stubs.mkdir()
        (stubs / "python3").write_text(
            "#!/bin/sh\n"
            'case "$*" in\n'
            f'  *docker-hardware*) printf "%s" "$3" > "{directory / 'hardware-output-path'}"; '
            f'echo docker-compose.yml; exit {hardware_exit} ;;\n'
            f'  *--emit-start-period*) printf derived > "{derive_marker}"; echo 2535s ;;\n'
            f'  *) printf "%s" "$PLURX_HEALTH_START_PERIOD" > "{proof_marker}"; exit {proof_exit} ;;\n'
            "esac\n",
            encoding="utf-8",
        )
        (stubs / "docker").write_text(
            "#!/bin/sh\n"
            'case "$*" in\n'
            '  *"compose config --images plurxd plurx-discovery"*) '
            'printf "%s\\n%s\\n" "$PLURX_IMAGE" '
            '"${STUB_DISCOVERY_IMAGE:-$PLURX_IMAGE}" ;;\n'
            '  *"compose config --images plurxd"*) echo registry.example/plurxd:test ;;\n'
            f'  *"compose pull plurxd"*) printf pulled > "{pull_marker}" ;;\n'
            '  *"org.opencontainers.image.revision"*) '
            'last=; for argument do last="$argument"; done; '
            'test "$last" = sha256:1111111111111111111111111111111111111111111111111111111111111111 '
            '|| exit 86; '
            'printf "%s\\n" "$STUB_IMAGE_REVISION" ;;\n'
            '  *"image inspect"*) '
            'echo sha256:1111111111111111111111111111111111111111111111111111111111111111 ;;\n'
            f'  *"compose up"*) printf "%s" "$PLURX_HEALTH_START_PERIOD" > "{up_marker}"; '
            f'printf "%s" "$PLURX_IMAGE" > "{image_marker}" ;;\n'
            "esac\n",
            encoding="utf-8",
        )
        (stubs / "git").write_text(
            "#!/bin/sh\n"
            'case "$*" in\n'
            '  "status --porcelain --untracked-files=no") '
            '[ "$STUB_TRACKED_DIRTY" = 1 ] && echo " M Makefile"; exit 0 ;;\n'
            '  "rev-parse HEAD") printf "%s\\n" "$STUB_CHECKOUT_REVISION" ;;\n'
            "  *) exit 99 ;;\n"
            "esac\n",
            encoding="utf-8",
        )
        for stub in stubs.iterdir():
            stub.chmod(0o755)
        environment = dict(os.environ, PATH=f"{stubs}:{os.environ['PATH']}")
        environment["STUB_IMAGE_REVISION"] = image_revision
        environment["STUB_CHECKOUT_REVISION"] = checkout_revision
        environment["STUB_TRACKED_DIRTY"] = "1" if tracked_dirty else "0"
        if discovery_image is not None:
            environment["STUB_DISCOVERY_IMAGE"] = discovery_image
        result = subprocess.run(
            ["sh", "-c", rollout],
            cwd=ROOT,
            check=False,
            text=True,
            env=environment,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        return (
            result.returncode,
            pull_marker,
            derive_marker,
            proof_marker,
            up_marker,
            image_marker,
        )

    def test_hardware_failure_stops_both_rollouts_and_removes_temporary_overlay(self):
        from unittest.mock import patch

        for target, command in (
            ("docker-up", "docker compose up -d --build"),
            ("docker-image-up", "docker compose up -d --no-build --pull never"),
        ):
            with self.subTest(target=target), tempfile.TemporaryDirectory() as temporary:
                # Inspect the real shell-owned overlay directory; positional
                # stub arguments change when preparation flags are added.
                # Parse Make before changing TMPDIR: macOS's SDK lookup may
                # create xcrun_db, which is unrelated to the shell's overlay.
                commands = make_dry_run_commands(target)
                with patch.dict(os.environ, {"TMPDIR": temporary}), patch(
                    __name__ + ".make_dry_run_commands", return_value=commands
                ):
                    code, pull, derive, proof, up, _ = self._run_rollout_recipe(
                        target, command, proof_exit=0, hardware_exit=2
                    )
                self.assertNotEqual(code, 0)
                # docker-up resolves its read-only startup budget before Pi
                # preparation; image-up performs hardware preflight first.
                self.assertEqual(derive.exists(), target == "docker-up")
                self.assertFalse(any(marker.exists() for marker in (pull, proof, up)))
                self.assertTrue((pull.parent / "hardware-output-path").exists())
                self.assertEqual(list(Path(temporary).iterdir()), [])

    def test_a_failed_budget_proof_stops_the_rollout_before_it_touches_a_container(
        self,
    ):
        # The proof used to be a make prerequisite, so make itself guaranteed a
        # failed check stopped the deploy. It is now `&&` inside one recipe, so
        # the guarantee is shell-level and has to be exercised: a `;` here
        # would let a refused budget deploy anyway, and no assertion about the
        # recipe's text catches that.
        code, pull_marker, derive_marker, proof_marker, up_marker, _ = (
            self._run_rollout_recipe(
                "docker-up", "docker compose up -d --build", proof_exit=2
            )
        )
        self.assertNotEqual(code, 0)
        self.assertFalse(pull_marker.exists())
        self.assertTrue(derive_marker.exists())
        self.assertTrue(proof_marker.exists())
        self.assertFalse(
            up_marker.exists(), "compose up ran after the budget proof failed"
        )

        code, pull_marker, derive_marker, proof_marker, up_marker, _ = (
            self._run_rollout_recipe(
                "docker-up", "docker compose up -d --build", proof_exit=0
            )
        )
        self.assertEqual(code, 0)
        self.assertFalse(pull_marker.exists())
        self.assertTrue(derive_marker.exists())
        self.assertEqual(proof_marker.read_text(encoding="utf-8"), "2535s")
        self.assertTrue(up_marker.exists())
        # The period that was proved is the period that gets applied.
        self.assertEqual(up_marker.read_text(encoding="utf-8"), "2535s")

    def test_prebuilt_image_rollout_matching_revision_proves_then_replaces(self):
        commands = make_dry_run_commands("docker-image-up")
        rollouts = [
            line
            for line in commands
            if "docker compose up -d --no-build --pull never" in line
        ]
        self.assertEqual(len(rollouts), 1)
        rollout = rollouts[0]
        self.assertTrue(rollout.startswith("cd deploy && "))
        self.assertEqual(rollout.count("docker compose pull plurxd"), 1)
        self.assertEqual(rollout.count("--emit-start-period"), 1)
        proof = (
            'PLURX_IMAGE="$image_id" PLURX_HEALTH_START_PERIOD="$period" python3 '
            "../scripts/validate-docker-startup-budget"
        )
        up = "docker compose up -d --no-build --pull never"
        self.assertIn(proof, rollout)
        self.assertLess(
            rollout.index("docker compose pull plurxd"),
            rollout.index("--emit-start-period"),
        )
        self.assertLess(rollout.index("--emit-start-period"), rollout.index(proof))
        self.assertLess(rollout.index(proof), rollout.index(up))
        self.assertIn(
            'PLURX_IMAGE="$image_id" PLURX_HEALTH_START_PERIOD="$period" PLURX_NODE_HOSTNAME=',
            rollout,
        )
        self.assertIn("docker image inspect --format '{{.Id}}'", rollout)
        self.assertIn('org.opencontainers.image.revision', rollout)
        self.assertIn('git status --porcelain --untracked-files=no', rollout)
        self.assertIn('git rev-parse HEAD', rollout)
        self.assertIn(
            'PLURX_IMAGE="$image_id" docker compose config --images plurxd plurx-discovery',
            rollout,
        )
        frozen_images = (
            'PLURX_IMAGE="$image_id" docker compose config --images '
            "plurxd plurx-discovery"
        )
        self.assertLess(
            rollout.index("org.opencontainers.image.revision"),
            rollout.index(frozen_images),
        )
        self.assertLess(
            rollout.index(frozen_images), rollout.index("--emit-start-period")
        )
        self.assertNotIn("PLURX_BUILD_REF", rollout)

        code, pull_marker, derive_marker, proof_marker, up_marker, _ = (
            self._run_rollout_recipe(
                "docker-image-up", up, proof_exit=2
            )
        )
        self.assertNotEqual(code, 0)
        self.assertTrue(pull_marker.exists(), "image was not pulled before the proof")
        self.assertTrue(derive_marker.exists())
        self.assertEqual(proof_marker.read_text(encoding="utf-8"), "2535s")
        self.assertFalse(
            up_marker.exists(), "compose up ran after the budget proof failed"
        )

        code, pull_marker, derive_marker, proof_marker, up_marker, image_marker = (
            self._run_rollout_recipe(
                "docker-image-up", up, proof_exit=0
            )
        )
        self.assertEqual(code, 0)
        self.assertTrue(pull_marker.exists())
        self.assertTrue(derive_marker.exists())
        self.assertEqual(proof_marker.read_text(encoding="utf-8"), "2535s")
        self.assertTrue(up_marker.exists())
        self.assertEqual(up_marker.read_text(encoding="utf-8"), "2535s")
        self.assertEqual(
            image_marker.read_text(encoding="utf-8"),
            "sha256:" + "1" * 64,
        )

        operations = read("docs/OPERATIONS.md")
        deploy_readme = read("deploy/README.md")
        for document in (operations, deploy_readme):
            self.assertIn("org.opencontainers.image.revision", document)
            self.assertIn("--no-build --pull never", document)
        self.assertNotIn("Restore `deploy/.env` to `latest`", operations)
        self.assertIn("restore\n`deploy/.env` to `main`", operations)

    def test_prebuilt_image_rollout_refuses_a_missing_revision_before_proof(self):
        up = "docker compose up -d --no-build --pull never"
        code, pull_marker, derive_marker, proof_marker, up_marker, _ = (
            self._run_rollout_recipe(
                "docker-image-up", up, proof_exit=0, image_revision=""
            )
        )

        self.assertNotEqual(code, 0)
        self.assertTrue(pull_marker.exists())
        self.assertFalse(derive_marker.exists())
        self.assertFalse(proof_marker.exists())
        self.assertFalse(up_marker.exists())

    def test_prebuilt_image_rollout_refuses_revision_mismatch_before_proof(self):
        up = "docker compose up -d --no-build --pull never"
        code, pull_marker, derive_marker, proof_marker, up_marker, _ = (
            self._run_rollout_recipe(
                "docker-image-up",
                up,
                proof_exit=0,
                image_revision="a" * 40,
                checkout_revision="b" * 40,
            )
        )

        self.assertNotEqual(code, 0)
        self.assertTrue(pull_marker.exists())
        self.assertFalse(derive_marker.exists())
        self.assertFalse(proof_marker.exists())
        self.assertFalse(up_marker.exists())

    def test_prebuilt_image_rollout_refuses_dirty_source_before_proof(self):
        up = "docker compose up -d --no-build --pull never"
        code, pull_marker, derive_marker, proof_marker, up_marker, _ = (
            self._run_rollout_recipe(
                "docker-image-up", up, proof_exit=0, tracked_dirty=True
            )
        )

        self.assertNotEqual(code, 0)
        self.assertTrue(pull_marker.exists())
        self.assertFalse(derive_marker.exists())
        self.assertFalse(proof_marker.exists())
        self.assertFalse(up_marker.exists())

    def test_prebuilt_image_rollout_refuses_divergent_discovery_image(self):
        up = "docker compose up -d --no-build --pull never"
        code, pull_marker, derive_marker, proof_marker, up_marker, _ = (
            self._run_rollout_recipe(
                "docker-image-up",
                up,
                proof_exit=0,
                discovery_image="registry.example/plurxd:other",
            )
        )

        self.assertNotEqual(code, 0)
        self.assertTrue(pull_marker.exists())
        self.assertFalse(derive_marker.exists())
        self.assertFalse(proof_marker.exists())
        self.assertFalse(up_marker.exists())

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
        self.assertIn("PLURX_BUILD_SHA=", rollout)
        self.assertIn("PLURX_NODE_HOSTNAME=", rollout)
        self.assertNotIn("-f deploy/docker-compose.yml", command)

        makefile = read("Makefile")
        self.assertIn("HOST_SHORTNAME := $(shell hostname -s", makefile)
        self.assertIn("BUILD_SHA := $(shell git rev-parse HEAD", makefile)
        self.assertIn('PLURX_BUILD_SHA="$(BUILD_SHA)"', makefile)
        self.assertIn('PLURX_NODE_HOSTNAME="$(HOST_SHORTNAME)"', makefile)

        dockerfile = read("Dockerfile")
        compose = read("deploy/docker-compose.yml")
        self.assertIn('ARG PLURX_BUILD_REF=""', dockerfile)
        self.assertIn("ENV PLURX_BUILD_REF=${PLURX_BUILD_REF}", dockerfile)
        self.assertIn('ARG PLURX_BUILD_SHA=""', dockerfile)
        self.assertIn("ENV PLURX_BUILD_SHA=${PLURX_BUILD_SHA}", dockerfile)
        self.assertIn("PLURX_BUILD_REF: ${PLURX_BUILD_REF:-}", compose)
        self.assertIn("PLURX_BUILD_SHA: ${PLURX_BUILD_SHA:-}", compose)

        cluster_build = read("crates/plurx-cluster-check/build.rs")
        self.assertIn('.filter(|value| !value.is_empty())', cluster_build)

        stages = list(re.finditer(r"(?im)^[ \t]*from\b.*$", dockerfile))
        runtime_stage = re.search(
            r"(?im)^[ \t]*from[ \t]+runtime-assets[ \t]+as[ \t]+runtime[ \t]*$",
            dockerfile,
        )
        self.assertIsNotNone(runtime_stage)
        assert runtime_stage is not None
        # The default build target (no --target, as compose builds it) must be
        # the shipped runtime: either the runtime stage itself is last, or the
        # last stage is a bare `FROM runtime AS <alias>` that adds nothing.
        if runtime_stage.start() != stages[-1].start():
            self.assertRegex(
                stages[-1].group(0), r"(?i)^[ \t]*from[ \t]+runtime[ \t]+as[ \t]+\S+[ \t]*$"
            )
            self.assertEqual(dockerfile[stages[-1].end() :].strip(), "")
        runtime = dockerfile[runtime_stage.end() :]
        self.assertIn('ARG PLURX_BUILD_SHA=""', runtime)
        self.assertIn(
            'LABEL org.opencontainers.image.revision="${PLURX_BUILD_SHA}"', runtime
        )
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
        default_transfer = re.search(
            r"(?m)^pub const DEFAULT_SNAPSHOT_TRANSFER_TIMEOUT_SECS: u64 = "
            r"([\d_]+);$",
            config_source,
        )
        default_install = re.search(
            r"(?m)^pub const DEFAULT_INSTALL_SNAPSHOT_TIMEOUT_SECS: u64 = "
            r"([\d_]+);$",
            config_source,
        )
        source_max_transfer = re.search(
            r"(?m)^pub const MAX_SNAPSHOT_TRANSFER_TIMEOUT_SECS: u64 = "
            r"([\d_]+);$",
            config_source,
        )
        source_max_install = re.search(
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
        self.assertIsNotNone(default_transfer)
        self.assertIsNotNone(default_install)
        self.assertIsNotNone(source_max_transfer)
        self.assertIsNotNone(source_max_install)
        self.assertTrue(all(value is not None for value in phases.values()))
        assert default_transfer is not None and default_install is not None
        assert source_max_transfer is not None and source_max_install is not None

        # Dockerfile owns the image default. Compose exposes independent
        # transfer/install overrides while the startup grace covers both.
        supported_default_startup_seconds = int(
            default_transfer.group(1).replace("_", "")
        ) + int(
            default_install.group(1).replace("_", "")
        ) + sum(int(value.group(1)) for value in phases.values() if value)
        self.assertGreaterEqual(
            start_period_seconds, supported_default_startup_seconds
        )

        env_example = read("deploy/.env.example")
        rollout_transfer = re.search(
            r"# PLURX_CLUSTER_SNAPSHOT_TRANSFER_TIMEOUT_SECS=(\d+)", env_example
        )
        rollout_install = re.search(
            r"# PLURX_CLUSTER_INSTALL_SNAPSHOT_TIMEOUT_SECS=(\d+)", env_example
        )
        rollout_health = re.search(
            r"# PLURX_HEALTH_START_PERIOD=(\d+)([smh])", env_example
        )
        self.assertIsNotNone(rollout_transfer)
        self.assertIsNotNone(rollout_install)
        self.assertIsNotNone(rollout_health)
        assert rollout_transfer is not None and rollout_install is not None
        assert rollout_health is not None
        rollout_transfer_seconds = int(rollout_transfer.group(1))
        rollout_install_seconds = int(rollout_install.group(1))
        self.assertEqual(
            rollout_transfer_seconds,
            int(default_transfer.group(1).replace("_", "")),
        )
        self.assertEqual(rollout_install_seconds, 1200)
        rollout_health_seconds = parse_duration(
            "".join(rollout_health.groups()), ".env.example rollout health period"
        )
        supported_rollout_startup_seconds = (
            rollout_transfer_seconds
            + rollout_install_seconds
            + sum(int(value.group(1)) for value in phases.values() if value)
        )
        self.assertGreaterEqual(
            rollout_health_seconds, supported_rollout_startup_seconds
        )

        supported_max_startup_seconds = int(
            source_max_transfer.group(1).replace("_", "")
        ) + int(source_max_install.group(1).replace("_", "")) + sum(
            int(value.group(1)) for value in phases.values() if value
        )
        self.assertEqual(supported_max_startup_seconds, 18135)
        self.assertIn("`5h3m`", read("docs/OPERATIONS.md"))

    def test_runtime_assets_layer_drops_build_time_state(self):
        """P-02 M6: the media runtime layer must not carry build-time state.

        Two cold builds of one commit differed in that layer only by apt,
        dpkg and alternatives logs, ldconfig's aux-cache, fontconfig's caches,
        the static probe's `config.log` and (across days) the account's
        last-change day. Each is either removed or given a fixed input.
        """
        dockerfile = read("Dockerfile")
        assets = dockerfile.split(" AS runtime-assets\n", 1)[1].split("\nFROM ", 1)[0]
        run = assets[assets.index("\nRUN ") :]
        removal = run.index("rm -rf /var/log/apt/* /var/log/*.log /var/cache/ldconfig/aux-cache")
        self.assertIn("/var/cache/fontconfig/*.cache-*", run[removal : removal + 200])
        # Removal comes after the last package operation in the layer.
        self.assertLess(run.rindex("apt-get autoremove -y"), removal)
        self.assertIn(
            'SOURCE_DATE_EPOCH=$(date -u -d "$snapshot_day" +%s) \\\n'
            "        useradd -r -g plurx -d /var/lib/plurx plurx",
            run,
        )
        self.assertIn("snapshot_day=$(printf '%s' \"$DEBIAN_SNAPSHOT\" | cut -c1-8)", run)
        # The commit's time would rebuild this layer on every commit.
        self.assertNotRegex(assets, r"(?m)^ARG SOURCE_DATE_EPOCH")

        probe = read("scripts/build-static-ffprobe")
        self.assertNotIn('"$documentation/config.log"', probe)
        self.assertIn('cp config.h "$documentation/config.h"', probe)
        self.assertIn('cp ffbuild/config.mak "$documentation/config.mak"', probe)

    def test_image_builds_pass_the_commit_time_as_source_date_epoch(self):
        """P-02 M6: two builds of one commit must be the same binary.

        `crates/plurxd/build.rs` used to stamp `built_at` from the compile
        clock, so no rebuild was ever byte-identical. It now honours
        `SOURCE_DATE_EPOCH`, which is only worth anything if every image build
        actually passes the commit's time: the Docker context has no `.git`.
        """
        dockerfile = read("Dockerfile")
        build_stage = dockerfile.split(" AS build\n", 1)[1].split("\nFROM ", 1)[0]
        self.assertRegex(build_stage, r"(?m)^ARG SOURCE_DATE_EPOCH$")
        self.assertLess(
            build_stage.index("ARG SOURCE_DATE_EPOCH"), build_stage.index("cargo build")
        )

        makefile = read("Makefile")
        self.assertIn(
            "SOURCE_DATE_EPOCH := $(if $(filter %-dirty,$(BUILD_REF)),,"
            "$(shell git log -1 --format=%ct 2>/dev/null))",
            makefile,
        )
        commit_time = subprocess.run(
            ["git", "log", "-1", "--format=%ct"],
            cwd=ROOT, check=True, text=True, stdout=subprocess.PIPE,
        ).stdout.strip()

        def docker_build(*variables: str, env: dict[str, str] | None = None) -> str:
            result = subprocess.run(
                ["make", "--no-print-directory", "-n", "docker", *variables],
                cwd=ROOT, check=True, text=True, stdout=subprocess.PIPE,
                env={**os.environ, **(env or {})},
            )
            (line,) = [c for c in result.stdout.splitlines() if "docker build" in c]
            return line

        # A clean tree is dated by HEAD, even with an unrelated value exported.
        clean = docker_build("BUILD_REF=v1.2.3", env={"SOURCE_DATE_EPOCH": "77"})
        self.assertIn(f'--build-arg SOURCE_DATE_EPOCH="{commit_time}"', clean)
        # A dirty tree is not HEAD: no date is passed, so build.rs reads the clock.
        dirty = docker_build("BUILD_REF=v1.2.3-4-gabc-dirty")
        self.assertIn('--build-arg SOURCE_DATE_EPOCH=""', dirty)
        (pinned,) = [
            c
            for c in make_dry_run_commands("docker", "SOURCE_DATE_EPOCH=1234")
            if "docker build" in c
        ]
        self.assertIn('--build-arg SOURCE_DATE_EPOCH="1234"', pinned)
        (rollout,) = [
            c
            for c in make_dry_run_commands("docker-up", "SOURCE_DATE_EPOCH=1234")
            if "docker compose up -d --build" in c
        ]
        self.assertIn('SOURCE_DATE_EPOCH="1234" ', rollout)
        self.assertIn(
            "SOURCE_DATE_EPOCH: ${SOURCE_DATE_EPOCH:-}", read("deploy/docker-compose.yml")
        )

        push = read("scripts/registry-push")
        self.assertIn('SOURCE_DATE_EPOCH="$(git log -1 --format=%ct "$FULL_SHA")"', push)
        self.assertIn('--build-arg SOURCE_DATE_EPOCH="$SOURCE_DATE_EPOCH"', push)

        smoke = workflow_job_blocks(".github/workflows/ci.yml")["package_smoke"]
        self.assertIn(
            'echo "source_date_epoch=$(git log -1 --format=%ct HEAD)" >> "$GITHUB_OUTPUT"',
            smoke,
        )
        self.assertIn(
            "SOURCE_DATE_EPOCH=${{ steps.binary-build.outputs.source_date_epoch }}", smoke
        )
        release = read(".github/workflows/publish-release.yml")
        self.assertIn(
            'echo "source_date_epoch=$(git show -s --format=%ct "$commit_sha")"', release
        )
        self.assertIn(
            "source_date_epoch: ${{ steps.release.outputs.source_date_epoch }}", release
        )
        self.assertIn(
            "SOURCE_DATE_EPOCH=${{ needs.resolve.outputs.source_date_epoch }}", release
        )

        build_rs = read("crates/plurxd/build.rs")
        self.assertIn("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH", build_rs)
        self.assertIn('std::env::var("SOURCE_DATE_EPOCH")', build_rs)

    def test_docker_build_frees_each_ffmpeg_download_before_the_next(self):
        dockerfile = read("Dockerfile")
        # Only the shipped media installer owns these two cache-clean points.
        # The CI tooling stage cleans its independently installed build tools.
        dockerfile = dockerfile.split("FROM runtime-assets AS ci", 1)[0]
        distro_install = dockerfile.index("intel-media-va-driver-non-free")
        first_clean = dockerfile.index("apt-get clean", distro_install)
        jellyfin_verify = dockerfile.index("sha256sum -c -", first_clean)
        jellyfin_install = dockerfile.index(
            'apt-get install -y --no-install-recommends "/tmp/${jellyfin_deb}"'
        )
        jellyfin_remove = dockerfile.index('rm -f "/tmp/${jellyfin_deb}"', jellyfin_install)
        second_clean = dockerfile.index("apt-get clean", jellyfin_install)

        self.assertLess(distro_install, first_clean)
        self.assertLess(first_clean, jellyfin_verify)
        self.assertLess(jellyfin_verify, jellyfin_install)
        self.assertLess(jellyfin_install, jellyfin_remove)
        self.assertLess(jellyfin_remove, second_clean)
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

    def test_docker_build_requires_the_atsc3_audio_decoder(self):
        dockerfile = read("Dockerfile")
        self.assertIn("jellyfin-ffmpeg8", dockerfile)
        self.assertIn("-decoders", dockerfile)
        self.assertIn("AC-4 decoder", dockerfile)

    def test_dockerfile_base_images_are_pinned_by_digest(self):
        # `rust:1-bookworm` and `debian:bookworm-slim` are moving tags. The
        # compiler was never the exposure -- `COPY . .` puts rust-toolchain.toml
        # in the build context and rustup honours it -- but the Debian system
        # libraries the daemon links against and the entire runtime userland
        # were whatever those tags pointed at on the day of the build. Two
        # images built from one commit could ship different libc, different
        # OpenSSL, and different fontconfig, and nothing in the tree would say
        # so.
        #
        # Digests are index digests (multi-platform), because the image is
        # built for amd64 and arm64 from the same `FROM`.
        dockerfile = read("Dockerfile")
        registry_bases = re.findall(
            r"(?m)^FROM\s+(\S+)\s+AS\s+(\S+)",
            dockerfile,
        )
        # `FROM runtime-assets AS runtime` names an earlier stage of this same
        # file, which has no registry reference to pin.
        stages = {stage for _, stage in registry_bases}
        # `scratch` is Docker's reserved empty base; it names no image and has
        # no digest to pin.
        unpinned = [
            (ref, stage)
            for ref, stage in registry_bases
            if ref not in stages and ref != "scratch" and "@sha256:" not in ref
        ]
        self.assertEqual(unpinned, [], "a base image is not pinned by digest")
        self.assertIn(
            "FROM rust:1-bookworm@sha256:"
            "93ce27a88655056a51dbdd8f5f2d7ddc071c7b0070fb288a37b5a285fc83971e AS build",
            dockerfile,
        )
        self.assertIn(
            "FROM debian:bookworm-slim@sha256:"
            "3783cc01769c7b2b1b83a5c5ad96c815348e28ed7da68e2e3687004faa906251"
            " AS runtime-assets",
            dockerfile,
        )

    def test_base_image_pin_drift_is_reported_weekly_and_gates_nothing(self):
        # A pin nobody looks at is a stale pin. The weekly dependency audit is
        # the job that already exists to say "upstream moved", so the report
        # rides it -- and rides it without a verdict, because updating the
        # userland the daemon ships is a decision, not a build failure.
        workflow = read(".github/workflows/rust-audit.yml")
        step = workflow.split("- name: Report base-image pin drift")
        self.assertEqual(len(step), 2, "the drift report step is missing")
        body = step[1].split("- name:")[0]
        self.assertIn("continue-on-error: true", body)
        # `continue-on-error` does not make a step run after an earlier one
        # failed; only a status condition does. Without it the report is
        # skipped in exactly the weeks the audit it rides on goes red.
        self.assertRegex(
            body,
            r"(?m)^\s+if: \$\{\{ (!cancelled\(\)|always\(\)) \}\}\s*$",
            "the drift report is skipped whenever an earlier audit step fails",
        )
        self.assertIn("scripts/image-base-drift Dockerfile", step[1])
        self.assertTrue((ROOT / "scripts/image-base-drift").exists())

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

        # The stage boundary, not the image reference: this test is about
        # what the runtime-assets stage contains and in what order, and
        # the base image now carries a digest that will be refreshed
        # without anything here changing meaning.
        runtime_assets_marker = " AS runtime-assets"
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
        # Cache-mounted target dirs must never serve a stale workspace crate.
        self.assertIn(
            "find crates vendor -type f -exec touch {} + \\\n"
            "    && ! cargo tree --locked -p plurxd",
            dockerfile,
        )
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
                "release-bin/*.dwp",
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
        # Android ships the signed release variant: the debug APK is
        # `debuggable`, which lets `adb shell run-as` read the bearer.
        self.assertIn(":app:assembleRelease", script)
        self.assertIn("apk/release/app-release.apk", script)
        self.assertNotIn(":app:assembleDebug", script)
        self.assertNotIn("app-debug.apk", script)

        # A matching build number cannot prove that the device has this
        # exact source tree; every reachable Apple device gets the archive.
        self.assertIn('devicectl device install app --device "$ident" "$app"', script)
        self.assertNotIn('already on build $APPLE_BUILD', script)

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

    def test_apple_developer_team_is_injected_instead_of_tracked(self):
        script = read("scripts/ship-physical")
        project = read("clients/apple/project.yml")
        export_options = read("clients/apple/ExportOptions.plist")

        assignment = re.search(
            r'^DEVELOPMENT_TEAM="\$\{PLURX_DEVELOPMENT_TEAM:-([^}]*)\}"$',
            script,
            re.MULTILINE,
        )
        self.assertIsNotNone(assignment)
        self.assertEqual(assignment.group(1), "")
        self.assertIn("PLURX_DEVELOPMENT_TEAM is required for Apple builds", script)
        self.assertIn('[[ "$DEVELOPMENT_TEAM" =~ ^[A-Z0-9]{10}$ ]]', script)
        self.assertIn('signed_team="$(codesign -d --verbose=4 "$artifact"', script)
        self.assertIn('if [[ "$signed_team" != "$DEVELOPMENT_TEAM" ]]', script)
        self.assertNotIn("DEVELOPMENT_TEAM:", project)
        self.assertNotIn("<key>teamID</key>", export_options)

    def test_publishing_documents_the_standalone_device_path(self):
        publishing = read("docs/PUBLISHING.md")
        self.assertIn("scripts/ship-physical", publishing)
        self.assertIn("When the controller cannot run the play", publishing)

    def test_ship_has_no_obsolete_lab4_port_exception(self):
        ship = read("scripts/ship")
        self.assertNotIn(
            "lab4's port is held by Plex; that is accepted, not a failure",
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
        self.assertNotIn("/Applications/Xcode_27.app/Contents/Developer", workflow)
        self.assertNotIn("brew install xcodegen", workflow)
        self.assertIn('grep -Fxq "Xcode 27.1"', workflow)
        self.assertIn('grep -Fxq "Build version 27A9275"', workflow)
        self.assertIn('test "$(xcrun --sdk iphoneos --show-sdk-version)" = "27.1"', workflow)
        self.assertIn('test "$(xcrun --sdk appletvos --show-sdk-version)" = "27.0"', workflow)
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
        self.assertIn("stale_ids=", workflow)
        self.assertIn('xcrun simctl shutdown "$stale_id"', workflow)
        self.assertIn('xcrun simctl delete "$stale_id"', workflow)
        self.assertIn('for simulator_id in "$ios_id" "$ipad_id" "$tvos_id"', workflow)
        self.assertIn('xcrun simctl boot "$simulator_id"', workflow)
        self.assertIn('xcrun simctl bootstatus "$simulator_id" -b', workflow)
        self.assertLess(workflow.index("stale_ids="), workflow.index("xcrun simctl create"))
        self.assertLess(workflow.index("xcrun simctl bootstatus"), workflow.index("run: make apple-test"))
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
        # DerivedData is reused by build/test destinations within this job only;
        # whole build state must not transfer between checkout roots.
        self.assertIn("APPLE_DERIVED_DATA := build/DerivedData", makefile)
        self.assertNotIn("path: clients/apple/build/DerivedData", workflow)

    def test_validation_workflows_do_not_compile_or_gate_windows(self):
        from validation.qualification import REQUIRED_JOBS

        self.assertNotIn("windows_compile", REQUIRED_JOBS)
        for path in (
            ".github/workflows/ci.yml",
            ".github/workflows/main-fast-lane.yml",
            ".github/workflows/effort-ci.yml",
        ):
            with self.subTest(workflow=path):
                workflow = read(path)
                self.assertNotIn("windows_compile", workflow)
                self.assertNotIn("WINDOWS_RESULT", workflow)
                self.assertNotIn("windows-cross", workflow)
                self.assertNotIn("cargo xwin", workflow)
                self.assertNotIn("x86_64-pc-windows", workflow)
                self.assertNotIn("scripts/package-windows", workflow)
                jobs = workflow_job_blocks(path)
                for name, block in jobs.items():
                    self.assertTrue(set(workflow_job_needs(block)) <= jobs.keys(), name)

    def test_pr_ci_selects_expensive_surfaces_and_has_one_aggregate_gate(self):
        workflow = read(".github/workflows/ci.yml")
        fast_lane = read(".github/workflows/main-fast-lane.yml")

        self.assertIn("python3 -m validation.ci_scope", workflow)
        self.assertIn("name: fast policy and contract preflight", workflow)
        self.assertIn("name: mobile release version", workflow)
        self.assertIn("name: Check mobile release hygiene", workflow)
        self.assertLess(
            workflow.index("name: Audit corrective-history evidence"),
            workflow.index("name: Check validation catalog and contract unit tests"),
        )
        self.assertIn("name: Main promotion gate", workflow)
        self.assertIn("branches: [main]", fast_lane)
        self.assertIn("name: Main promotion gate", fast_lane)
        self.assertIn("effort/*|integration/*-into-main)", workflow)
        self.assertIn("scope_event=effort_qualification", workflow)
        self.assertIn("qualification: ${{ steps.scope.outputs.qualification }}", workflow)
        metadata_jobs = workflow_job_blocks(".github/workflows/ci.yml")
        for name in ("scope", "pr_gate"):
            self.assertRegex(metadata_jobs[name], r"(?m)^    timeout-minutes: 10$")
        fast_rust = workflow.split("  rust_compile:", 1)[1].split(
            "\n  check:", 1
        )[0]
        unit_rust = workflow.split("  check:", 1)[1].split(
            "\n  cluster_store_legacy:", 1
        )[0]
        self.assertIn("name: fast Rust gate", fast_rust)
        # Full run 5274 exhausted twenty minutes during all-target checking;
        # the compiler lanes share the effort's bounded thirty-minute allowance.
        self.assertIn("timeout-minutes: 30", fast_rust)
        metadata_jobs = workflow_job_blocks(".github/workflows/ci.yml")
        for compiler_job in (
            metadata_jobs["rust_compile"],
            metadata_jobs["cluster_wal"],
            workflow_job_blocks(".github/workflows/effort-ci.yml")["rust_compile"],
        ):
            compiler_header = compiler_job.split("    steps:", 1)[0]
            self.assertIn(
                "runs-on: [self-hosted, Linux, X64, lab, baremetal, ci-topology]",
                compiler_header,
            )
            self.assertRegex(
                compiler_header,
                r'(?m)^      CARGO_BUILD_JOBS: "1"$',
            )
        self.assertIn("run: make effort-rust-check", fast_rust)
        self.assertNotIn("cargo test", fast_rust)
        fast_finalize = fast_rust.split(
            "- name: Enforce persistent Cargo bounds", 1
        )[1]
        self.assertIn("continue-on-error: true", fast_finalize)
        self.assertIn("name: Rust unit and SQLite contracts", unit_rust)
        self.assertIn("run: make ci-rust-gate", unit_rust)
        self.assertNotIn("scripts/validate run", fast_rust)
        self.assertNotIn("actions/setup-node", fast_rust)
        self.assertNotIn("./.github/actions/playwright", fast_rust)
        self.assertNotIn("uses: ./.github/actions/ffmpeg", fast_rust)
        self.assertIn("uses: ./.github/actions/ffmpeg", unit_rust)
        self.assertIn('major: "8"', unit_rust)
        self.assertIn("binary: /usr/lib/jellyfin-ffmpeg/ffmpeg", unit_rust)
        # The shipped-runtime qualification uses FFmpeg 8; the independent
        # main fast lane retains its pinned FFmpeg 6 compiler environment.
        self.assertIn(
            'major: "6"',
            workflow_job_blocks(".github/workflows/main-fast-lane.yml")["rust_compile"],
        )
        # Membership alone would stay green with the step moved below the gate
        # it provisions, which is exactly the failure this contract records.
        self.assertLess(
            unit_rust.index("uses: ./.github/actions/ffmpeg"),
            unit_rust.index("run: make ci-rust-gate"),
        )
        self.assertIn("if: needs.scope.outputs.apple == 'true'", workflow)
        web_layout = workflow.split("\n  web_layout:", 1)[1].split(
            "\n  android_jvm:", 1
        )[0]
        android_device = workflow.split("\n  android_device:", 1)[1].split(
            "\n  package_smoke:", 1
        )[0]
        self.assertIn("if: needs.scope.outputs.web_layout == 'true'", web_layout)
        self.assertIn("if: needs.scope.outputs.android_device == 'true'", android_device)
        vod_web = workflow.split("\n  vod_web:", 1)[1].split(
            "\n  web_layout:", 1
        )[0]
        self.assertIn("needs: [scope, preflight, web_layout]", vod_web)
        self.assertNotIn("group: plurx-browser-heavy", web_layout)
        self.assertNotIn("group: plurx-browser-heavy", vod_web)
        self.assertIn("--case suspend-resume", vod_web)
        self.assertIn("docs/streaming/VOD-STEADY-ACCEPTANCE-HANDOFF.md", vod_web)
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
        windows_action = read(".github/actions/windows-cross/action.yml")
        self.assertIn("cargo-xwin --version", windows_action)
        self.assertIn("cargo install cargo-xwin --version 0.23.1 --locked", windows_action)
        self.assertIn('echo "XWIN_ARCH=x86_64"', windows_action)
        self.assertIn('echo "XWIN_CRT_VERSION=14.44.17.14"', windows_action)
        self.assertIn('echo "XWIN_HTTP_RETRIES=8"', windows_action)
        self.assertIn('echo "XWIN_SDK_VERSION=10.0.26100"', windows_action)
        self.assertIn('echo "XWIN_VARIANT=desktop"', windows_action)
        self.assertIn('echo "XWIN_VERSION=17"', windows_action)
        self.assertIn('} >> "$GITHUB_ENV"', windows_action)
        self.assertNotIn("XWIN_CACHE_DIR", windows_action)
        self.assertNotIn("cargo xwin cache xwin", windows_action)
        self.assertIn("command -v clang-cl", windows_action)
        self.assertIn('echo "$tool_dir" >> "$GITHUB_PATH"', windows_action)
        self.assertIn(
            "cargo xwin build --workspace --all-targets --locked", windows_action
        )
        self.assertIn("--target x86_64-pc-windows-msvc", windows_action)
        self.assertIn('CARGO_BUILD_JOBS: "1"', windows_action)
        self.assertIn('CARGO_PROFILE_DEV_DEBUG: "0"', windows_action)
        self.assertIn('CARGO_PROFILE_TEST_DEBUG: "0"', windows_action)
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

        # The coverage workflow owns measurement and publication independently
        # of the manually dispatched full-CI sweep.
        self.assertNotIn("coverage", workflow_job_blocks(".github/workflows/ci.yml"))

        readme = read("README.md")
        for badge in (
            "../badges-pr-ci/ci.svg",
            "../badges-pr-lint/lint.svg",
            "../badges/coverage.svg",
        ):
            self.assertIn(badge, readme)
        self.assertEqual(
            sum(line.startswith("[![") for line in readme.splitlines()),
            3,
        )
        self.assertNotIn("../badges-ci/ci.svg", readme)
        self.assertNotIn("../badges-lint/lint.svg", readme)
        self.assertNotIn("docs/img/badges/", readme)
        self.assertNotRegex(
            readme,
            r"https?://[^)]+/actions/workflows/[^)]+/badge\.svg",
        )
        self.assertNotIn("http://forge.lan:3000", readme)
        self.assertNotRegex(
            readme,
            r"(?:https?://|ssh://git@)forge\.lan(?:[:/]|$)",
        )
        self.assertNotIn("img.shields.io/endpoint", readme)
        self.assertNotIn("raw.githubusercontent.com/pjunod/plurx/badges", readme)

        badge = workflow_job_blocks(".github/workflows/ci.yml")["badge"]
        self.assertIn("if: always()", badge)
        self.assertIn("scripts/publish-badge", badge)
        self.assertIn("--branch badges-ci", badge)
        self.assertIn("--message \"$message\"", badge)
        self.assertIn("success|skipped", badge)

        fast_gate = workflow_job_blocks(".github/workflows/main-fast-lane.yml")[
            "promotion_gate"
        ]
        step = "Require the reviewed head to remain current"
        self.assertIn(step, fast_gate)
        # Behaviour, not text: run the step itself against scratch repositories.
        # The jobs build the PR head alone, so the gate refuses a moved head
        # and accepts any movement of the base branch.
        currency = workflow_step_blocks(fast_gate)[step]
        script = textwrap.dedent(currency.split("        run: |\n", 1)[1])
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)

            def git(*args: str, cwd: Path = root / "work") -> str:
                return subprocess.run(
                    ["git", "-c", "user.name=t", "-c", "user.email=t@t", *args],
                    cwd=cwd, check=True, capture_output=True, text=True,
                ).stdout.strip()

            (root / "work").mkdir()
            git("init", "-q", "-b", "main")
            git("commit", "-q", "--allow-empty", "-m", "base")
            git("checkout", "-q", "-b", "topic")
            git("commit", "-q", "--allow-empty", "-m", "tested head")
            tested = git("rev-parse", "HEAD")
            git("init", "-q", "--bare", str(root / "origin.git"), cwd=root)
            git("remote", "add", "origin", str(root / "origin.git"))
            git("push", "-q", "origin", "topic:refs/pull/7/head")

            def gate(pr_number: str = "7", head: str = tested) -> int:
                return subprocess.run(
                    ["bash", "--noprofile", "--norc", "-e", "-o", "pipefail", "-c", script],
                    cwd=root / "work", capture_output=True, text=True,
                    env={**os.environ, "PR_NUMBER": pr_number,
                         "EVENT_HEAD_SHA": head, "GITHUB_TOKEN": "token"},
                ).returncode

            self.assertEqual(gate(), 0, "an unchanged head passes")
            git("checkout", "-q", "main")
            git("commit", "-q", "--allow-empty", "-m", "main advanced")
            git("push", "-q", "origin", "main")
            self.assertEqual(gate(), 0, "an advanced base does not invalidate the lane")
            git("checkout", "-q", "--orphan", "rewritten")
            git("commit", "-q", "--allow-empty", "-m", "rewritten main")
            git("push", "-q", "--force", "origin", "rewritten:main")
            self.assertEqual(gate(), 0, "the base is not what the jobs built")
            git("checkout", "-q", "topic")
            git("commit", "-q", "--allow-empty", "-m", "untested push")
            git("push", "-q", "--force", "origin", "topic:refs/pull/7/head")
            self.assertNotEqual(gate(), 0, "a head pushed after the lane started is refused")
            self.assertNotEqual(gate(pr_number="", head=""), 0, "only a pull request can pass")
        self.assertIn("--branch badges-pr-ci", fast_gate)
        self.assertIn("--branch badges-pr-lint", fast_gate)
        self.assertIn("if: needs.rust_compile.result == 'success'", fast_gate)
        self.assertLess(
            fast_gate.index(step),
            fast_gate.index("Publish the qualified PR gate badge"),
        )

        package = workflow_job_blocks(".github/workflows/ci.yml")["package_smoke"]
        self.assertNotIn("needs: check", package)
        self.assertIn("needs: [scope, preflight]", package)

        preflight = workflow_job_blocks(".github/workflows/ci.yml")["preflight"]
        operations_run = workflow_step_blocks(preflight)["Check CI, deploy, and shipping contracts"]
        self.assertEqual(workflow_step_literal(operations_run, "run"), [
            'if [ "$RETAIN_OPERATIONS_UNITS" = true ]; then',
            '  echo "Adopted authenticated complete operations unit discovery"',
            "else", "  make operations-check", "fi",
        ])
        self.assertIn("RETAIN_OPERATIONS_UNITS: ${{ needs.scope.outputs.retain_operations_units }}", operations_run)
        effort_preflight = workflow_job_blocks(".github/workflows/effort-ci.yml")[
            "preflight"
        ]
        main_preflight = workflow_job_blocks(".github/workflows/main-fast-lane.yml")[
            "preflight"
        ]
        for workflow, contract_preflight, unit_command in (
            ("ci", preflight, "make operations-check"),
            ("effort-ci", effort_preflight, "run: python3 -m validation.python_unit_receipts run"),
            ("main-fast-lane", main_preflight, "run: python3 -m validation.main_unit_receipts run"),
        ):
            self.assertIn(
                "uses: https://data.forgejo.org/actions/setup-node@"
                "49933ea5288caeca8642d1e84afbd3f7d6820020",
                contract_preflight,
            )
            self.assertIn('node-version: "22.23.2"' if workflow == "main-fast-lane"
                          else 'node-version: "22"', contract_preflight)
            self.assertLess(
                contract_preflight.index(
                    "actions/setup-node@49933ea5288caeca8642d1e84afbd3f7d6820020"
                ),
                contract_preflight.index(unit_command),
            )
            # The shared player-input fixtures compile into no Rust and no
            # client on a fixture-only diff, so without this step a ruling
            # could be edited out of the contract with nothing to notice.
            if workflow == "effort-ci":
                self.assertNotIn("node tests/playback/player-input-contract.test.js", contract_preflight)
                self.assertNotIn("node tests/web/player-dom.test.js", contract_preflight)
            elif workflow == "main-fast-lane":
                player_step = workflow_step_blocks(contract_preflight)[
                    "Check the shared player input contract"
                ]
                self.assertEqual(
                    workflow_step_scalar(player_step, "run"),
                    "python3 -m validation.main_preflight_adoption node",
                )
                self.assertIn("GITHUB_TOKEN: ${{ github.token }}", player_step)
                # Only the attributable adapter executes these scripts; an
                # additional direct invocation would replay a retained pass.
                self.assertNotIn("node tests/playback/player-input-contract.test.js", contract_preflight)
                self.assertNotIn("node tests/web/player-dom.test.js", contract_preflight)
            else:
                self.assertIn("node tests/playback/player-input-contract.test.js", contract_preflight)
                self.assertIn("node tests/web/player-dom.test.js", contract_preflight)

        lint = read(".github/workflows/lint.yml")
        self.assertNotIn("\n  pull_request:\n", lint)
        self.assertNotIn("\n  merge_group:\n", lint)
        self.assertIn("workflow_dispatch:", lint)
        self.assertIn("run: make fmt-check lint", lint)
        lint_job = workflow_job_blocks(".github/workflows/lint.yml")["lint"]
        lint_steps = workflow_step_blocks(lint_job)
        self.assertIn("continue-on-error: true", lint_steps["Run rustfmt and Clippy"])
        self.assertIn("if: always()", lint_steps["Publish the lint result"])
        self.assertIn("--branch badges-lint", lint_steps["Publish the lint result"])
        self.assertIn(
            "if: steps.lint.outcome != 'success'",
            lint_steps["Preserve the lint verdict"],
        )

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

    def test_manual_effort_scope_enables_every_compile_surface(self):
        from validation.ci_scope import resolve_scope

        scope = workflow_step_blocks(
            workflow_job_blocks(".github/workflows/effort-ci.yml")["scope"]
        )["Select affected compile surfaces"]
        self.assertIn("--event workflow_dispatch", scope)
        self.assertNotIn("--base", scope)
        selected = resolve_scope("workflow_dispatch", None)
        for surface in ("rust", "apple", "android_jvm", "web_layout"):
            self.assertTrue(selected[surface], surface)
        self.assertFalse(selected["docs_only"])

    def test_coverage_refreshes_on_main_without_full_ci(self):
        workflow = read(".github/workflows/coverage.yml")
        trigger = workflow.split("\non:\n", 1)[1].split("\nenv:", 1)[0]
        self.assertNotIn("  push:", trigger)
        self.assertIn("  workflow_dispatch:", trigger)
        self.assertNotIn("pull_request", trigger)
        self.assertNotIn("paths", trigger)
        self.assertIn("group: coverage-main", workflow)
        self.assertIn("cancel-in-progress: true", workflow)
        jobs = workflow_job_blocks(".github/workflows/coverage.yml")
        self.assertEqual(set(jobs), {"coverage"})
        coverage = jobs["coverage"]
        self.assertEqual(workflow_job_needs(coverage), ())
        self.assertIn("if: github.ref == 'refs/heads/main'", coverage)
        self.assertIn("ref: main", coverage)
        self.assertIn("persist-credentials: false", coverage)
        self.assertIn("components: llvm-tools-preview", coverage)
        self.assertIn("lane: coverage", coverage)
        self.assertIn('major: "6"', coverage)
        self.assertIn(
            "cargo llvm-cov --workspace --locked --exclude plurx-cluster-check",
            coverage,
        )
        self.assertIn("--failure-mode all", coverage)
        steps = workflow_step_blocks(coverage)
        publish = steps["Publish the unit coverage badge"]
        self.assertNotIn("continue-on-error", publish)
        self.assertNotIn("if: always()", publish)
        self.assertIn("scripts/publish-badge", publish)
        self.assertIn("--branch badges", publish)
        self.assertIn("--alias codex/badges", publish)
        self.assertIn("date -u +%F", publish)
        self.assertIn('--message "${msg}% · ${measured_on} UTC"', publish)
        self.assertLess(
            coverage.index("Measure line coverage"),
            coverage.index("Publish the unit coverage badge"),
        )
        self.assertIn("Enforce persistent Cargo bounds", steps)
        for path in (ROOT / ".github/workflows").glob("*.yml"):
            if path.name != "coverage.yml":
                self.assertNotIn("--filename coverage", path.read_text())

    def test_coverage_uses_general_runner_with_pinned_ffmpeg_container(self):
        # Retain this historical regression anchor after reserving its pool.
        coverage = workflow_job_blocks(".github/workflows/coverage.yml")["coverage"]
        self.assertIn(
            "runs-on: [self-hosted, Linux, X64, lab, ci-coverage]\n",
            coverage,
        )
        self.assertIn("image: ubuntu:24.04", coverage)
        self.assertIn("options: --cpus 2 --memory 6g --memory-swap 10g", coverage)
        self.assertIn('CARGO_BUILD_JOBS: "1"', coverage)
        self.assertIn('RUST_TEST_THREADS: "2"', coverage)
        steps = workflow_step_blocks(coverage)
        install = steps["Install coverage prerequisites"]
        for package in (
            "git", "jq", "nodejs", "python3", "build-essential", "clang",
            "libssl-dev", "libgtk-4-dev", "libdbus-1-dev",
        ):
            self.assertRegex(install, rf"\b{package}\b")
        self.assertLess(
            coverage.index("Install coverage prerequisites"),
            coverage.index("actions/checkout@"),
        )
        self.assertIn('major: "6"', coverage)
        self.assertLess(
            coverage.index("./.github/actions/ffmpeg"),
            coverage.index("Measure line coverage"),
        )
        cleanup = steps["Restore persistent runner workspace ownership"]
        self.assertIn("if: always()", cleanup)
        self.assertIn('chown -R "$owner" "$GITHUB_WORKSPACE"', cleanup)

    def test_manual_qualification_dispatch_inputs_have_explicit_string_types(self):
        dispatch = read(".github/workflows/ci.yml").split(
            "  workflow_dispatch:\n", 1
        )[1].split("\nenv:", 1)[0]
        self.assertTrue(dispatch.startswith("    inputs:\n"))
        starts = list(re.finditer(r"(?m)^      ([a-z_][a-z0-9_]*):\n", dispatch))
        descriptions = {
            "retained_units_sha256": "Authenticated digest-addressed retained unit proof; no suite replay",
            "retained_units_attachment_id": "Private promotion PR evidence attachment ID",
            "retained_units_attachment_uuid": "Private promotion PR evidence attachment UUID",
            "promotion_pr": "Optional open same-repo effort/integration promotion PR to main",
            "promotion_head_sha": "Immutable promotion head SHA; dispatch on that head branch",
            "promotion_base_sha": "Immutable current main SHA already merged into the promotion head",
        }
        self.assertEqual([entry.group(1) for entry in starts], list(descriptions))
        for index, entry in enumerate(starts):
            with self.subTest(input=entry.group(1)):
                end = starts[index + 1].start() if index + 1 < len(starts) else len(dispatch)
                fields = re.findall(r"(?m)^        ([a-z_]+): (.+)$", dispatch[entry.end():end])
                self.assertEqual(len(fields), 4, "dispatch inputs require exactly one explicit renderer type")
                self.assertEqual(dict(fields), {
                    "type": "string",
                    "description": '"' + descriptions[entry.group(1)] + '"',
                    "required": "false",
                    "default": '""',
                })
    def test_effort_compile_only_policy_preserves_required_checks_and_claims_no_units(self):
        workflow = read(".github/workflows/effort-ci.yml")
        inputs = workflow.split("    inputs:\n", 1)[1].split("\nenv:\n", 1)[0]
        self.assertRegex(inputs, r"compile_only:[\s\S]*type: boolean[\s\S]*default: false")
        jobs = workflow_job_blocks(".github/workflows/effort-ci.yml")
        steps = workflow_step_blocks(jobs["preflight"])
        for name in (
            "Restore attributable Python successes",
            "Publish Python attempt-start marker",
            "Provision checksummed public K08 oracle sources",
            "Check CI, deploy, and shipping contracts",
        ):
            with self.subTest(unit_step=name):
                self.assertIn("if: ${{ !inputs.compile_only }}", steps[name])
        self.assertIn("steps.receipts.outcome == 'success'", steps["Preserve Python success journal even on unit failure"])
        self.assertNotIn("compile_only", steps["Audit corrective-history evidence"])
        catalog_step = steps["Check validation catalog and contract unit tests"].split("      - uses:", 1)[0]
        self.assertIn("make validation-lint", catalog_step)
        self.assertNotIn("compile_only", catalog_step)
        for job in ("rust_compile", "web_static", "apple_compile", "android_compile", "effort_gate"):
            with self.subTest(required_job=job):
                self.assertNotIn("compile_only", jobs[job])
        for path in (".github/workflows/ci.yml", ".github/workflows/main-fast-lane.yml"):
            self.assertNotIn("compile_only", read(path), "main qualification has no task-unit bypass")

    def test_native_macos_tray_is_independent_of_retained_mobile_evidence(self):
        def enabled(expression, *, apple, desktop, retained, tv_adopted=False):
            values = {"apple": apple, "desktop_macos": desktop, "retain_apple": retained}
            expression = re.sub(
                r"(steps\.native_receipts\.outputs\.tv_navigation != 'true')|"
                r"needs\.scope\.outputs\.(apple|desktop_macos|retain_apple) (==|!=) 'true'",
                lambda match: str(not tv_adopted) if match[1] else
                str(values[match[2]] if match[3] == "==" else not values[match[2]]),
                expression)
            expression = expression.replace("always()", "True").replace("&&", " and ").replace("||", " or ")
            self.assertRegex(expression, r"^[ ()TrueFalsandor]+$")
            return eval(expression, {"__builtins__": {}}, {})

        for path, job_name in (
            (".github/workflows/effort-ci.yml", "apple_compile"),
            (".github/workflows/main-fast-lane.yml", "apple_compile"),
            (".github/workflows/ci.yml", "apple"),
        ):
            with self.subTest(path=path):
                jobs = workflow_job_blocks(path)
                self.assertIn("desktop_macos: ${{ steps.scope.outputs.desktop_macos }}", jobs["scope"])
                job = jobs[job_name]
                condition = re.search(r"(?m)^    if: (.+)$", job)[1]
                steps = workflow_step_blocks(job)
                desktop_name = ("Compile native macOS server tray" if "effort-ci" in path
                                else "Build and test native macOS server tray")
                desktop = steps[desktop_name]
                # Retained mobile evidence never suppresses the independent tray step.
                # Only its own authenticated native proof can adopt tray operations.
                self.assertTrue(enabled(condition, apple=False, desktop=True, retained=True))
                self.assertTrue(enabled(workflow_step_scalar(desktop, "if"),
                                        apple=False, desktop=True, retained=True))
                self.assertFalse(enabled(condition, apple=False, desktop=False, retained=False))
                self.assertTrue(enabled(condition, apple=True, desktop=False, retained=False))
                self.assertFalse(enabled(workflow_step_scalar(desktop, "if"),
                                         apple=True, desktop=False, retained=False))
                if path.endswith("main-fast-lane.yml"):
                    receipts = steps["Authenticate compatible original native test receipts"]
                    self.assertNotIn("if:", receipts)
                    self.assertIn("run: python3 -m validation.native_receipts", receipts)
                    adoption = steps["Preserve native adoption provenance"]
                    self.assertEqual(workflow_step_scalar(adoption, "if"), "always()")
                    self.assertIn("path: .main-preflight-receipts/native/receipt.json", adoption)
                    self.assertIn("name: native-adoption-${{ github.run_id }}-${{ github.run_attempt }}", adoption)
                    tv_step = steps["Require native TV navigation journeys"]
                    tv_guard = workflow_step_scalar(tv_step, "if")
                    self.assertTrue(enabled(tv_guard, apple=True, desktop=False, retained=False,
                                            tv_adopted=False))
                    self.assertFalse(enabled(tv_guard, apple=True, desktop=False, retained=False,
                                             tv_adopted=True))
                    tv_artifact = steps["Preserve TV navigation evidence including failures"]
                    tv_artifact_guard = workflow_step_scalar(tv_artifact, "if")
                    self.assertTrue(enabled(tv_artifact_guard, apple=True, desktop=False, retained=False,
                                            tv_adopted=False))
                    self.assertFalse(enabled(tv_artifact_guard, apple=True, desktop=False, retained=False,
                                             tv_adopted=True))
                self.assertNotIn("xcodegen", steps["Verify the pinned Apple toolchain"])
                for name, step in steps.items():
                    if path.endswith("main-fast-lane.yml") and name in {
                        "Authenticate compatible original native test receipts",
                        "Preserve native adoption provenance",
                    }:
                        continue
                    if path.endswith("main-fast-lane.yml") and name in {
                        "Compile iOS and tvOS without running simulator tests",
                        "Require native TV navigation journeys",
                        "Preserve TV navigation evidence including failures",
                    }:
                        guard = workflow_step_scalar(step, "if")
                        for tv_adopted in (False, True):
                            self.assertFalse(enabled(guard, apple=False, desktop=True, retained=True,
                                                     tv_adopted=tv_adopted), name)
                        self.assertTrue(enabled(guard, apple=True, desktop=False, retained=False,
                                                tv_adopted=False), name)
                        self.assertFalse(enabled(guard, apple=True, desktop=False, retained=False,
                                                 tv_adopted=True), name)
                        continue
                    if name == "Preserve native macOS server tray check output":
                        self.assertTrue(enabled(workflow_step_scalar(step, "if"),
                                                apple=False, desktop=True, retained=True))
                        self.assertFalse(enabled(workflow_step_scalar(step, "if"),
                                                 apple=True, desktop=False, retained=False))
                        continue
                    if name == "Verify the pinned Apple toolchain":
                        self.assertNotIn("if:", step)
                        self.assertIn('grep -Fxq "Build version 27A9275"', step)
                        continue
                    if name == desktop_name:
                        continue
                    # Existing mobile work and its always-run artifacts/cleanup are
                    # conditional, so a tray-only selection creates no simulators.
                    guard = workflow_step_scalar(step, "if")
                    self.assertFalse(enabled(guard, apple=False, desktop=True, retained=True), name)
                    self.assertTrue(enabled(guard, apple=True, desktop=False, retained=False), name)
                    if path.endswith("main-fast-lane.yml"):
                        self.assertTrue(enabled(guard, apple=True, desktop=False, retained=False,
                                                tv_adopted=True), name)
                    if path.endswith("/ci.yml"):
                        self.assertFalse(enabled(guard, apple=True, desktop=True, retained=True), name)

    def test_native_macos_tray_unit_output_does_not_pollute_mobile_receipts(self):
        case_line = "Test Case '-[PlurxTrayCoreTests.ProtocolTests testFixture]' passed (0.001 seconds)."
        for path, name in ((".github/workflows/main-fast-lane.yml", "apple_compile"),
                           (".github/workflows/ci.yml", "apple")):
            step = workflow_step_blocks(workflow_job_blocks(path)[name])[
                "Build and test native macOS server tray"]
            raw_commands = "\n".join(workflow_step_literal(step, "run"))
            adoption_modes = (False, True) if path.endswith("main-fast-lane.yml") else (False,)
            for adopted in adoption_modes:
                commands = raw_commands.replace(
                    "${{ steps.native_receipts.outputs.desktop_macos }}", str(adopted).lower())
                for status in (0, 7):
                    with self.subTest(path=path, adopted=adopted, status=status), tempfile.TemporaryDirectory() as directory:
                        root = Path(directory)
                        fake_make = root / "make"
                        args_file = root / "make-args"
                        fake_make.write_text(
                            "#!/bin/sh\nprintf '%s\\n' \"$*\" > \"$NATIVE_TRAY_ARGS\"\nprintf '%s\\n' \"" +
                            case_line + "\"\nexit \"$NATIVE_TRAY_FAKE_EXIT\"\n", encoding="utf-8")
                        fake_make.chmod(0o755)
                        log = root / "target/validation/desktop-macos-check.log"
                        log.parent.mkdir(parents=True)
                        log.write_text("Test Suite 'All tests' passed\nExecuted 61 tests, with 0 failures (0 unexpected)\n")
                        completed = subprocess.run(
                            ["/bin/sh", "-eu", "-c", commands], cwd=root,
                            env={**os.environ, "PATH": str(root) + os.pathsep + os.environ["PATH"],
                                 "NATIVE_TRAY_FAKE_EXIT": str(status),
                                 "NATIVE_TRAY_ARGS": str(args_file)},
                            capture_output=True, text=True, check=False)
                        self.assertEqual(completed.returncode, 0 if adopted else status)
                        self.assertNotIn("Test Case", completed.stdout + completed.stderr)
                        log = root / "target/validation/desktop-macos-check.log"
                        if adopted:
                            self.assertFalse(args_file.exists(), "adopted builds and tests invoke no make target")
                            self.assertIn("builds and tests adopted", log.read_text())
                            self.assertIn("native-adoption artifact", log.read_text())
                            self.assertNotIn("Test Suite", log.read_text())
                            self.assertNotIn("Test Case", log.read_text())
                            from validation.native_receipts import verify_desktop
                            from validation.python_unit_receipts import ReceiptError
                            with self.assertRaises(ReceiptError):
                                verify_desktop(log.read_bytes())
                            self.assertIn("builds and tests adopted", completed.stdout)
                        else:
                            self.assertEqual(args_file.read_text().strip(), "desktop-macos-check")
                            self.assertEqual(log.read_text().strip(), case_line)
                        if not adopted and status == 0:
                            self.assertIn("checks passed", completed.stdout)
                        elif not adopted:
                            self.assertNotIn("tests adopted", completed.stdout)
                            self.assertNotIn("checks passed", completed.stdout)

    def test_effort_native_macos_tray_compiles_without_running_units(self):
        effort = workflow_job_blocks(".github/workflows/effort-ci.yml")["apple_compile"]
        steps = workflow_step_blocks(effort)
        desktop = steps["Compile native macOS server tray"]
        self.assertEqual(workflow_step_scalar(desktop, "run"), "make desktop-macos-build")
        self.assertNotIn("compile_only", desktop)
        self.assertNotIn("desktop-macos-check", effort)
        self.assertNotIn("desktop-macos-test", effort)
        self.assertNotIn("swift test", effort)

    def test_native_macos_tray_checks_build_both_architectures_and_run_swift_tests(self):
        makefile = read("Makefile")
        build = makefile.split("\ndesktop-macos-build:", 1)[1].split("\ndesktop-macos-test:", 1)[0]
        test = makefile.split("\ndesktop-macos-test:", 1)[1].split("\ndesktop-macos-check:", 1)[0]
        self.assertIn("for arch in arm64 x86_64", build)
        self.assertIn("clients/server-tray/macos/build --configuration release", build)
        self.assertIn('--arch "$$arch"', build)
        self.assertIn('--scratch "$(CURDIR)/target/desktop-macos/scratch/$$arch"', build)
        self.assertNotIn("swift test", build)
        self.assertIn("swift test --package-path clients/server-tray/macos", test)
        self.assertNotIn("--arch", test, "tests execute only on the actual runner architecture")
        self.assertIn("desktop-macos-check: desktop-macos-build desktop-macos-test", makefile)
        for path, name in ((".github/workflows/main-fast-lane.yml", "apple_compile"),
                           (".github/workflows/ci.yml", "apple")):
            steps = workflow_step_blocks(workflow_job_blocks(path)[name])
            desktop = steps["Build and test native macOS server tray"]
            commands = workflow_step_literal(desktop, "run")
            commands = [line.strip() for line in commands if line.strip()]
            if path.endswith("main-fast-lane.yml"):
                self.assertIn('if [ "${{ steps.native_receipts.outputs.desktop_macos }}" = "true" ]; then', commands)
                self.assertFalse(any("make desktop-macos-build" in command for command in commands))
                self.assertTrue(any("builds and tests adopted" in command and "> target/validation/desktop-macos-check.log" in command for command in commands))
                self.assertIn("else", commands)
                self.assertIn("make desktop-macos-check > target/validation/desktop-macos-check.log 2>&1", commands)
                self.assertTrue(any("tests adopted" in line for line in commands))
                self.assertTrue(any("checks passed" in line for line in commands))
            else:
                self.assertIn("make desktop-macos-check > target/validation/desktop-macos-check.log 2>&1", commands)
            self.assertFalse(any("tee " in command or "cat " in command for command in commands),
                             "desktop XCTest records must not enter the mobile receipt log")
            artifact = steps["Preserve native macOS server tray check output"]
            self.assertIn("path: target/validation/desktop-macos-check.log", artifact)
            self.assertIn("if-no-files-found: error", artifact)

    def test_main_qualification_is_full_and_effort_prs_are_compile_only(self):
        workflow = read(".github/workflows/ci.yml")
        fast_lane = read(".github/workflows/main-fast-lane.yml")
        effort = read(".github/workflows/effort-ci.yml")
        effort_jobs = workflow_job_blocks(".github/workflows/effort-ci.yml")
        effort_rust_steps = workflow_step_blocks(effort_jobs["rust_compile"])
        # Whole-job budget includes checkout/toolchain/cache setup plus the
        # unchanged all-target compile and vendored denied-warning Clippy.
        full_jobs = workflow_job_blocks(".github/workflows/ci.yml")
        for lane, rust_job, minutes in (
            ("effort", effort_jobs["rust_compile"], 30),
            ("full compiler", full_jobs["rust_compile"], 30),
        ):
            with self.subTest(lane=lane):
                self.assertRegex(rust_job, rf"(?m)^    timeout-minutes: {minutes}$")
                rust_env = re.search(r"(?ms)^    env:\n(.*?)(?=^    \S)", rust_job)
                self.assertIsNotNone(rust_env, "bound Cargo concurrency at job scope")
                self.assertEqual(
                    [line.strip() for line in rust_env.group(1).splitlines()
                     if line.strip() and not line.strip().startswith("#")],
                    ['CARGO_BUILD_JOBS: "1"'],
                )
        full_compile_steps = workflow_step_blocks(full_jobs["rust_compile"])
        self.assertEqual(workflow_step_scalar(
            full_compile_steps["Compile every Rust target without executing tests"], "run"
        ), "make effort-rust-check")
        self.assertRegex(full_jobs["check"], r"(?m)^    timeout-minutes: 120$")
        fast_jobs = workflow_job_blocks(".github/workflows/main-fast-lane.yml")
        # Both receipt-producing preflights retain a finite fifteen-minute
        # budget so the full history/contracts can publish their evidence.
        self.assertIn("timeout-minutes: 15", fast_jobs["preflight"])
        self.assertIn("timeout-minutes: 15", effort_jobs["preflight"])
        self.assertIn("timeout-minutes: 30", effort_jobs["rust_compile"])
        effort_rust_header = effort_jobs["rust_compile"].split("\n    steps:", 1)[0]
        self.assertIn('    env:\n      CARGO_BUILD_JOBS: "1"', effort_rust_header)
        self.assertEqual(
            re.findall(r"(?m)^[ \t]*CARGO_BUILD_JOBS: [^\n]+$", effort_jobs["rust_compile"]),
            ['      CARGO_BUILD_JOBS: "1"'],
        )
        self.assertIn(
            "if: always() && steps.cargo-cache.outputs.local == 'true'",
            effort_rust_steps["Enforce persistent Cargo bounds"],
        )
        fast_rust_steps = workflow_step_blocks(fast_jobs["rust_compile"])
        lint = read(".github/workflows/lint.yml")
        makefile = read("Makefile")
        precommit = read("scripts/pre-commit")
        catalog = tomllib.loads(read("validation/points.toml"))
        membership_web_tests = read("tests/web/cluster-membership.test.js")

        # Forgejo has no GitHub merge-queue event. The single required
        # aggregate workflow fires on every ready main-bound pull request -
        # no label, so no way to merge one the lane never saw. Runtime sweeps
        # are manual or release-tag only.
        self.assertNotIn("\n  merge_group:\n", workflow)
        self.assertNotIn("\n  merge_group:\n", lint)
        self.assertNotIn("\n  pull_request:\n", lint)
        self.assertNotIn("\n  pull_request:\n", workflow)
        self.assertIn("\n  workflow_dispatch:\n", workflow)
        self.assertIn("\n  pull_request:\n", fast_lane)
        self.assertIn("branches: [main]", fast_lane)
        self.assertIn("name: Main promotion gate", fast_lane)

        # Effort compilation is explicitly dispatched and executes none of the
        # slow release suites.
        self.assertNotIn("\n  pull_request:\n", effort)
        self.assertIn("\n  workflow_dispatch:\n", effort)
        self.assertIn("name: Effort development gate", effort)
        self.assertEqual(
            list(effort_rust_steps),
            [
                "Install Rust gate prerequisites",
                "Check out the candidate",
                "Install the pinned Rust toolchain",
                "Restore the effort Rust cache",
                "Compile every Rust target without executing tests",
                "Enforce persistent Cargo bounds",
                "Restore persistent runner workspace ownership",
            ],
        )
        rust_toolchain = effort_rust_steps["Install the pinned Rust toolchain"]
        self.assertEqual(
            workflow_step_scalar(rust_toolchain, "uses"),
            "https://github.com/dtolnay/rust-toolchain@4716b85f2fac3e324e64fa2810f6b5c3905760a5",
        )
        self.assertIn("          components: rustfmt, clippy", rust_toolchain)
        self.assertEqual(
            workflow_step_literal(
                effort_rust_steps["Compile every Rust target without executing tests"],
                "run",
            ),
            ["make effort-rust-check", "make hiqlite-vendor-clippy"],
        )
        # The deadline itself is pinned above with the larger-memory queue.
        self.assertIn('CARGO_BUILD_JOBS: "1"', effort_jobs["rust_compile"])
        self.assertEqual(
            list(fast_rust_steps),
            [
                "Install Rust gate prerequisites",
                "Check out the candidate",
                "Install the pinned Rust toolchain",
                "Install the pinned FFmpeg",
                "Restore the main fast Rust cache",
                "Compile every Rust target without executing tests",
                "Lint the workspace",
                "Enforce persistent Cargo bounds",
                "Restore persistent runner workspace ownership",
            ],
        )
        self.assertIn("container: ubuntu:24.04", fast_jobs["rust_compile"])
        prerequisites = workflow_step_literal(
            fast_rust_steps["Install Rust gate prerequisites"], "run"
        )
        self.assertTrue(any("python3" in line for line in prerequisites))
        self.assertTrue(any("nodejs" in line for line in prerequisites))
        self.assertEqual(
            workflow_step_literal(fast_rust_steps["Lint the workspace"], "run"),
            ["make lint"],
        )
        self.assertEqual(
            workflow_step_literal(
                fast_rust_steps["Compile every Rust target without executing tests"],
                "run",
            ),
            ["make effort-rust-check", "make hiqlite-vendor-clippy"],
        )
        # Full-suite execution belongs to manual CI. Exact step/command lists
        # keep a renamed unit step or an appended broad test command from
        # silently returning to the blocking PR lane.
        self.assertNotIn("make unit", fast_jobs["rust_compile"])
        self.assertNotIn("cargo test", fast_jobs["rust_compile"])
        self.assertIn('major: "6"', fast_rust_steps["Install the pinned FFmpeg"])
        self.assertIn("timeout-minutes: 60", fast_jobs["rust_compile"])
        fast_preflight = workflow_step_blocks(fast_jobs["preflight"])
        playback_contracts = workflow_step_scalar(
            fast_preflight["Check the shared player input contract"], "run"
        )
        self.assertEqual(playback_contracts, "python3 -m validation.main_preflight_adoption node")
        adapter = read("validation/main_preflight_adoption.py")
        # Parse the actual producer inventory rather than accepting script
        # names that occur only in a comment or another unrelated function.
        import ast

        producer = ast.parse(adapter)
        node_assignments = [
            node for node in producer.body
            if isinstance(node, ast.Assign)
            and any(isinstance(target, ast.Name) and target.id == "NODE"
                    for target in node.targets)
        ]
        self.assertEqual(len(node_assignments), 1)
        self.assertEqual(
            ast.literal_eval(node_assignments[0].value),
            (
                "tests/playback/player-input-contract.test.js",
                "tests/playback/web-policy.test.js",
                "tests/playback/web-control.test.js",
                "tests/web/seek-telemetry.test.js",
                "tests/web/player-dom.test.js",
                "tests/web/read-after.test.js",
                "tests/web/live-tv.test.js",
            ),
        )
        node_phase = adapter.split('    if phase == "node":', 1)[1].split(
            '    in_progress = {"phase": phase,', 1
        )[0]
        self.assertIn("for script in NODE:", node_phase)
        self.assertIn('if identity in journal["outcomes"]:', node_phase)
        self.assertIn(
            'command = ["node", "--test", script] if script.endswith("seek-telemetry.test.js") else ["node", script]',
            node_phase,
        )
        self.assertIn("subprocess.run(command, check=True)", node_phase)
        self.assertIn('record(identity, "success")', node_phase)
        self.assertIn("cancel-in-progress: false", fast_lane)
        self.assertEqual(
            make_dry_run_commands("hiqlite-vendor-clippy"),
            [
                "cargo clippy --locked --manifest-path vendor/hiqlite/Cargo.toml "
                "--lib --no-default-features "
                "--features auto-heal,cache,macros,sqlite -- -D warnings"
            ],
        )
        vendor_clippy = next(
            check
            for check in catalog["checks"]
            if check["id"] == "hiqlite-vendor-clippy"
        )
        self.assertEqual(
            vendor_clippy,
            {
                "id": "hiqlite-vendor-clippy",
                "title": (
                    "Vendored Hiqlite production snapshot transport "
                    "denied-warning Clippy"
                ),
                "command": "make hiqlite-vendor-clippy",
                "profiles": ["commit", "ci", "full", "nightly"],
                "requires": ["cargo", "make"],
                "missing": "fail",
                "timeout_seconds": 1800,
            },
        )
        unit_core = next(
            check for check in catalog["checks"] if check["id"] == "unit-core"
        )
        self.assertEqual(unit_core["expect_at_least"], 1100)
        self.assertEqual(
            unit_core["command"],
            "PLURX_EXPECT_TEST_COUNT_AT_LEAST=1100 make unit-core",
        )
        self.assertIn(
            "$(CARGO) test --locked -p plurx-core --features hiqlite-store --lib",
            makefile,
        )
        self.assertIn("PLURX_EXPECT_TEST_COUNT_AT_LEAST", makefile)
        core_point = next(
            point for point in catalog["points"] if point["id"] == "core.media"
        )
        self.assertIn("unit-core", core_point["checks"])
        cluster_point = next(
            point for point in catalog["points"] if point["id"] == "cluster.auth"
        )
        self.assertIn("hiqlite-vendor-clippy", cluster_point["checks"])
        self.assertIn(
            "benchmarks/cluster-transport-recovery-role.schema.json",
            cluster_point["paths"],
        )
        self.assertIn("run: make apple-build", effort)
        self.assertIn("run: make android", effort)
        self.assertIn("run: make effort-web-static-check", effort)
        web_check = makefile.split(".PHONY: web-check", 1)[1].split(".PHONY:", 1)[0]
        self.assertIn("web-check: web-unit-check ", web_check)
        web_unit_check = makefile.split(".PHONY: web-unit-check", 1)[1].split(".PHONY:", 1)[0]
        self.assertIn("node tests/web/cluster-membership.test.js", web_unit_check)
        self.assertIn(
            'test("transport recovery is attributed only to the receiving node"',
            membership_web_tests,
        )
        self.assertIn(
            'test("active transport stays visible through its server-projected deadline"',
            membership_web_tests,
        )
        self.assertIn(
            'test("transport observer age advances the repaint projection"',
            membership_web_tests,
        )
        self.assertIn(
            'test("rejected public and wrong-observer transport evidence cannot reappear"',
            membership_web_tests,
        )
        self.assertNotIn("make ci-rust-gate", effort)
        self.assertNotIn("make cluster-", effort)
        self.assertNotIn("make apple-test", effort)
        self.assertNotIn("make android-test", effort)
        self.assertNotIn("android-instrumentation", effort)
        self.assertNotIn("ui-check", effort)
        self.assertNotIn("playback-lab", effort)
        self.assertNotIn("docker/build-push-action", effort)
        self.assertIn("$(CARGO) check --workspace --locked --all-targets", makefile)
        self.assertIn("ROOT=$(git rev-parse --show-toplevel)", precommit)
        self.assertIn("make precommit-check", precommit)
        self.assertNotIn("PLURX_EFFORT_COMMIT", precommit)
        self.assertNotIn("history-check", precommit)
        self.assertNotIn("operations-check", precommit)
        self.assertNotIn("scripts/validate run", precommit)
        precommit_gate = makefile.split(".PHONY: precommit-check", 1)[1].split(
            ".PHONY:", 1
        )[0]
        self.assertIn(
            "precommit-check: validation-lint fmt-check lint",
            precommit_gate,
        )
        self.assertIn("scripts/js-check", precommit_gate)

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
        harness_commands = make_dry_run_commands("cluster-harness-check")
        snapshot_launch_tests = (
            "a_legacy_launch_retains_the_production_snapshot_policy",
            "a_snapshot_threshold_round_trips_and_reaches_the_launched_voter_config",
            "an_invalid_snapshot_threshold_is_rejected_without_changing_production",
        )
        for test_name in snapshot_launch_tests:
            command = next(
                (command for command in harness_commands if test_name in command),
                None,
            )
            self.assertIsNotNone(
                command,
                f"cluster-harness-check does not run {test_name}",
            )
            self.assertIn("scripts/require-test-count", command)
        wal_run = workflow_step_blocks(jobs["cluster_wal"])["Run exact Hiqlite and WAL recovery regressions"]
        self.assertEqual(workflow_step_literal(wal_run, "run"), [
            'if [ "$RETAIN_WAL_UNITS" = true ]; then',
            "  python3 -m validation.qualification_unit_continuation --compile-units wal",
            "else", "  make cluster-wal-check", "fi",
        ])
        self.assertIn("RETAIN_WAL_UNITS: ${{ needs.scope.outputs.retain_wal_units }}", wal_run)
        self.assertIn("PLURX_RETAINED_UNITS: ${{ needs.scope.outputs.retained_units }}", wal_run)
        wal_commands = make_dry_run_commands("cluster-wal-check")
        self.assertTrue(
            all("scripts/require-test-count" in command for command in wal_commands),
            "every cluster WAL command must retain the nonzero test-count guard",
        )
        hiqlite_snapshot_tests = (
            "raw_write_frame_stays_idle_after_transport_recovers_until_an_explicit_flush",
            "deadline_less_retry_serializes_its_effective_fallback_stall_boundary",
            "real_tls_tail_backpressure_completes_for_every_payload_direction",
            "real_tls_no_backpressure_control_completes_without_an_extra_frame",
            "frame_completion_flushes_the_underlying_transport",
            "flush_failure_is_a_failed_frame_write",
            "permanently_blocked_flush_expires_the_combined_budget",
            "close_frame_uses_the_short_fixed_budget",
            "production_raft_request_writer_flushes_a_snapshot_chunk_through_tls",
            "production_raft_writer_reports_flush_failure_to_its_supervisor",
            "production_raft_response_writer_flushes_serialized_response_through_tls",
            "production_raft_response_writer_reports_flush_failure_and_closes",
            "production_api_request_writer_flushes_serialized_request_through_tls",
            "production_api_writer_reports_flush_failure_to_its_supervisor",
            "queued_leader_change_wins_before_queued_api_request",
            "queued_api_response_wins_before_queued_leader_change",
            "queued_api_response_wins_before_leader_during_writer_backpressure",
            "leader_handoff_settles_decoded_response_before_failing_unresolved_requests",
            "dedicated_leader_control_bypasses_application_backlog_during_writer_backpressure",
            "proxy_handoff_settles_decoded_response_before_failing_unresolved_requests",
            "dropped_caller_still_allows_its_proxy_refusal_to_claim_handoff",
            "concurrent_proxy_refusals_coalesce_into_one_endpoint_advance",
            "teardown_drained_proxy_refusal_claims_the_socket_handoff",
            "teardown_drained_non_proxy_refusal_preserves_terminal_ambiguity",
            "production_api_response_writer_flushes_serialized_response_through_tls",
            "production_api_response_writer_reports_flush_failure_and_closes",
            "production_raft_reader_reports_malformed_frames",
            "raft_transport_debug_logging_exposes_only_bounded_metadata",
            "production_api_reader_reports_malformed_frames",
            "silent_peer_cannot_hold_the_server_handshake_open",
            "executor_runs_one_queues_one_and_drops_abandoned_queued_work",
            "shutdown_reports_running_work_without_cancelling_it",
            "cancelled_shutdown_wait_retains_the_executor_handle",
            "shutdown_before_submission_is_latched_and_admits_no_work",
            "admission_deadline_never_executes_the_timed_out_job",
            "accepted_partial_write_finishes_before_same_offset_retry",
            "production_snapshot_executor_worker_records_status_around_injected_installer",
            "production_snapshot_executor_fifo_status_ignores_later_admission_waiter",
            "production_snapshot_executor_ticket_order_survives_reverse_waiter_polling",
            "production_snapshot_status_follows_worker_start_after_pre_ticket_deschedule",
            "admission_waiter_budget_bounds_cancelled_ticket_retention_and_reports_busy",
            "production_shared_executor_retains_abandoned_other_peer_terminal_status",
            "inbound_result_disposition_distinguishes_mismatch_higher_vote_and_fatal",
            "handler_coordinator_consumes_retained_reset_when_request_queue_is_full",
            "replacement_socket_drops_cancelled_request_after_consuming_reset",
            "live_request_on_stale_socket_requires_reconnect",
            "reset_interrupts_write_enqueue_under_backpressure",
            "latched_reset_prevents_write_queue_ownership_transfer",
            "latched_reader_failure_prevents_write_queue_ownership_transfer",
            "shutdown_interrupts_write_enqueue_under_backpressure",
            "handler_coordinator_observes_writer_failure_while_reader_is_pending",
            "handler_coordinator_treats_writer_panic_as_terminal",
            "handler_coordinator_observes_reader_failure_while_writer_is_pending",
            "handler_coordinator_treats_reader_panic_as_terminal",
            "forced_reset_cleanup_does_not_wait_for_full_writer_queue",
            "dropping_connection_off_runtime_keeps_cleanup_owned",
            "repeated_connection_failures_return_supervised_tasks_to_baseline",
            "production_connection_supervisor_does_not_invent_snapshot_work",
            "production_natural_reconnect_advances_physical_socket_epoch_without_reset",
            "production_snapshot_catch_up_preserves_physical_socket_epoch_during_interleaving",
            "two_client_churn_cannot_overwrite_the_snapshot_connection_owner",
            "sqlite_install_snapshot_preserves_mismatch_for_offset_reset",
            "cache_install_snapshot_preserves_mismatch_for_offset_reset",
            "snapshot_chunk_deadlines_advance_without_renewing_the_transfer_window",
            "non_final_snapshot_rpc_uses_hard_ttl_when_it_is_shorter_than_chunk_budget",
            "final_install_deadline_latches_once_and_mismatch_restores_transfer_deadline",
            "final_install_respects_the_first_rpc_hard_cap_and_transfer_expiry",
            "mismatch_cannot_reenter_final_install_after_transfer_expiry",
            "watchable_snapshot_deadline_switches_to_the_active_phase",
            "simultaneous_final_phase_update_wins_over_stale_transfer_timer",
            "production_final_mismatch_restores_transfer_deadline_before_reread",
            "production_final_transport_error_retains_final_install_deadline_in_status",
            "production_higher_vote_response_never_advances_snapshot_acknowledgement",
            "sqlite_full_snapshot_enters_bounded_wrapper",
            "cache_full_snapshot_enters_bounded_wrapper",
            "advancing_transfer_may_exceed_one_chunk_window_and_finish_before_transfer_expiry",
            "unanswered_non_final_rpc_expires_at_chunk_budget_and_resets_socket",
            "repeated_mismatch_style_resets_expire_at_the_original_transfer_deadline",
            "final_install_may_exceed_chunk_window_but_cannot_renew_install_window",
            "stale_snapshot_guard_cannot_clear_a_newer_attempt",
            "caller_cancellation_drops_the_active_snapshot_rpc_guard_immediately",
            "aborting_snapshot_owner_publishes_attempt_guarded_terminal_status",
            "outbound_snapshot_attempt_bounds_identity_before_supervisor_logging",
            "snapshot_deadline_durations_are_bounded_before_instant_arithmetic",
            "outbound_snapshot_identity_is_bounded_before_retention",
            "inbound_snapshot_identity_is_bounded_before_retention",
            "outbound_attempted_offset_never_rolls_back_within_one_snapshot_attempt",
            "bounded_inbound_display_id_cannot_alias_semantic_snapshot_identity",
            "snapshot_fingerprint_correlates_raw_identity_across_directions_without_display_aliasing",
            "snapshot_fingerprint_is_optional_for_rolling_status_deserialization",
            "local_status_distinguishes_receive_ack_install_retry_and_completion",
            "receiver_bytes_are_not_reported_as_sender_acknowledgements",
            "inbound_progress_is_monotonic_and_new_identity_clears_attempt_state",
            "same_peer_inbound_install_and_outbound_transfer_do_not_collide",
            "terminal_wrapper_preserves_the_actionable_failure_category",
            "specific_terminal_failure_replaces_prior_transient_category",
            "outbound_live_socket_counts_the_first_reconnect",
            "production_supervisor_start_before_snapshot_attempt_counts_first_reconnect",
            "stale_inbound_completion_cannot_mutate_newer_snapshot_identity",
            "abandoned_inbound_admission_does_not_displace_worker_result",
            "inbound_socket_epoch_and_reconnect_count_are_attempt_local",
            "configured_inbound_inter_chunk_deadline_uses_minimum_and_maximum",
            "configured_peer_capacity_covers_both_groups_and_directions",
            "live_membership_add_remove_churn_rekeys_capacity_and_retired_allowance",
            "active_observation_remains_visible_through_deadline_then_expires",
            "live_install_longer_than_five_minutes_remains_visible_through_its_deadline",
            "configured_chunk_deadline_controls_awaiting_ack_stall_projection",
            "completed_observation_never_projects_as_stalled_and_eventually_expires",
            "valid_install_does_not_stall_at_the_chunk_window",
            "retired_peers_are_bounded",
            "production_transport_route_enforces_auth_and_returns_memory_only_json",
            "production_transport_client_uses_exact_authenticated_route_and_accepts_404",
            "snapshot_transport_peer_tracks_current_raft_membership_for_new_learner",
            "production_transport_client_rejects_oversized_unframed_response_body",
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

        daemon_transport_tests = (
            "peer_fanout_applies_the_one_second_per_peer_deadline",
            "peer_fanout_never_exceeds_the_eight_peer_bound",
            "production_collector_labels_directory_overflow_without_probing_it",
            "peer_status_cache_is_fresh_for_five_seconds_then_expires",
            "full_refresh_cycle_keeps_cache_fresh_and_ages_transport_from_local_cache_time",
            "remote_status_freshness_uses_local_monotonic_age_despite_clock_skew",
            "stale_peer_sample_uses_local_elapsed_time_and_cannot_become_healthy",
            "absent_and_expired_cache_are_unavailable_never_peer_limited",
            "membership_status_cache_is_fresh_for_five_seconds_then_expires",
            "membership_projection_refresh_is_bounded_and_preserves_last_good_sample",
            "long_install_crosses_to_stalled_then_expires_after_the_post_deadline_window",
            "cached_fresh_active_transport_stalls_exactly_when_deadline_reaches_zero",
            "cached_deadline_less_active_transport_uses_the_producer_fallback_boundary",
            "cached_terminal_transport_observation_expires_at_five_minutes",
            "transport_projection_uses_local_monotonic_age_despite_remote_clock_skew",
            "peer_status_refresh_keeps_strict_cycle_overhead_margin",
            "aggregate_request_reads_only_node_owned_projections",
            "production_collector_preserves_private_transport_when_public_listener_is_closed",
            "slow_public_probe_cannot_discard_completed_private_transport",
            "identity_mismatch_discards_peer_local_state",
            "public_transport_fallback_requires_matching_observer_identity",
            "unresolved_exact_release_keeps_admissions_fenced_past_the_lease_deadline",
            "restart_cancellation_cannot_clear_a_maintenance_owned_fence",
            "delayed_same_owner_cleanup_cannot_clear_a_successor_generation",
            "overlapping_cleanup_tokens_are_resolved_independently",
            "confirmed_release_resolves_latch_but_preserves_timed_fence",
            "confirmed_current_restart_cancel_resolves_a_preexisting_exact_latch",
            "confirmed_current_restart_cancel_resolves_an_expired_exact_latch",
            "confirmed_maintenance_exit_resolves_an_expired_exact_latch",
            "confirmed_maintenance_exit_preserves_a_restart_owned_latch",
            "stuck_restart_admission_cannot_outlive_the_exact_fence",
            "mutation_preflight_ages_local_transport_across_peer_collection",
            "restart_preparation_claims_and_releases_the_replicated_outage_slot",
            "cancelled_waiter_keeps_the_serialized_operation_gate_with_its_owner",
            "cleanup_retry_backoff_is_capped_and_shutdown_interruptible",
            "credential_revocation_uses_the_exact_committed_security_roster",
            "credential_revocation_accepts_more_than_the_diagnostics_probe_limit",
            "membership_added_between_begin_passes_is_fenced_before_store_admission",
            "replicated_membership_exclusion_spans_final_roster_read_and_peer_end",
            "replicated_exclusion_projection_outlives_remote_ttl_and_clock_skew",
            "cache_admin_revocation_operation_queue_is_bounded_and_raii_released",
            "local_apply_ack_wire_version_rejects_pre_barrier_receivers",
            "begin_ack_installs_memory_fence_before_waiting_for_exact_local_apply",
            "cancelled_local_apply_wait_leaves_peer_memory_fence_closed",
            "origin_waits_for_exact_claim_apply_before_observing_added_member",
            "automatic_activation_runs_outside_the_membership_projection",
            "full_roster_projection_activates_without_a_credential_mutation",
            "permanent_activation_stops_the_one_time_worker",
            "stuck_automatic_activation_does_not_block_membership_refresh_or_shutdown",
            "automatic_activation_checks_the_permanent_marker_before_taking_the_gate",
        )
        for test_name in daemon_transport_tests:
            matching = [command for command in wal_commands if test_name in command]
            self.assertEqual(len(matching), 1, test_name)
            command = matching[0]
            self.assertIn("cargo test --locked -p plurxd --bin plurxd", command)
            self.assertIn("-- --exact", command)

        core_snapshot_tests = (
            "snapshot_install_timeout_is_bounded_and_env_values_are_parsed",
            "snapshot_chunk_and_transfer_timeouts_validate_bounds_and_relationship",
            "snapshot_budget_env_overrides_are_parsed_and_empty_values_do_not_override",
            "operations_peer_directory_preserves_identities_beyond_the_probe_limit",
            "operations_peer_query_materializes_only_the_committed_roster",
            "cache_admin_revocation_roster_includes_pending_removals_and_fails_on_omission",
            "membership_cannot_commit_after_final_roster_read_before_credential_store_write",
            "definitive_cache_admin_acquire_release_cycles_leave_no_receipts",
            "definitive_cache_admin_singleton_losers_leave_no_receipts",
            "zero_after_ambiguous_cache_admin_acquire_advances_watermark",
            "ambiguous_cache_admin_acquire_advances_watermark_and_cannot_resurrect",
            "repeated_ambiguous_cache_admin_cleanup_keeps_one_bounded_watermark",
            "server_commit_response_loss_and_crash_expire_cache_admin_exclusion_conservatively",
            "cache_admin_exclusion_is_separate_rolling_safe_and_capability_v4",
            "active_cache_revocation_exclusion_blocks_readiness_without_wall_clock_expiry",
            "cache_revocation_capability_keeps_a_joiner_closed_until_self_is_committed",
            "rollback_heartbeat_cannot_republish_retired_cache_revocation_capabilities",
            "rollback_credential_mutation_is_rejected_before_readiness_refresh",
            "three_voter_rolling_upgrade_activates_credential_guard_only_after_full_roster",
            "activated_guard_with_live_mutation_lease_is_not_an_activation_candidate",
            "status_protocol_query_materializes_only_the_committed_roster",
            "committed_roster_bound_fails_closed_instead_of_truncating",
            "released_planned_outage_claim_cannot_be_resurrected_by_a_delayed_write",
            "maintenance_and_exact_release_are_safe_in_both_commit_orders",
            "ambiguous_operation_acquire_retries_only_while_its_owned_claim_is_live",
            "previous_release_lifecycle_writes_cannot_cross_an_outage_lease",
            "production_timing_admits_recovery_after_upgrade_and_clean_rolling_restarts",
            "immediate_watermark_errors_cannot_starve_due_startup_log",
        )
        for test_name in core_snapshot_tests:
            matching = [command for command in wal_commands if test_name in command]
            self.assertEqual(len(matching), 1, test_name)
            command = matching[0]
            self.assertIn("cargo test --locked -p plurx-core", command)
            self.assertIn("--lib -- --exact", command)

        core_store_tests = (
            "password_and_session_revocation_roll_back_together_on_delete_failure",
            "failed_combined_promotion_never_authorizes_the_old_session",
            "concurrent_admin_demote_and_delete_preserve_one_administrator",
        )
        for test_name in core_store_tests:
            matching = [command for command in wal_commands if test_name in command]
            self.assertEqual(len(matching), 1, test_name)
            command = matching[0]
            self.assertIn("cargo test --locked -p plurx-core --lib", command)
            self.assertIn("-- --exact", command)

        clustered_store_tests = (
            "clustered_promotion_requires_the_exact_claim_and_rolls_back_on_token_failure",
            "login_token_insert_requires_the_verified_password_version",
        )
        for test_name in clustered_store_tests:
            matching = [command for command in wal_commands if test_name in command]
            self.assertEqual(len(matching), 1, test_name)
            command = matching[0]
            self.assertIn(
                "--features cluster-read-cost-validation,hiqlite-contract-tests",
                command,
            )
            self.assertIn("--test store_contract", command)
            self.assertIn("RUST_MIN_STACK=8388608", command)
            self.assertIn("-- --exact --test-threads=1", command)

        proxy_writer_test = (
            "server::proxy::stream::tests::"
            "production_proxy_response_writer_flushes_serialized_response_through_tls"
        )
        matching = [command for command in wal_commands if proxy_writer_test in command]
        self.assertEqual(len(matching), 1)
        self.assertIn("--no-default-features --features server", matching[0])
        self.assertIn("--lib -- --exact", matching[0])

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
        snapshot_metrics = [
            command
            for command in wal_commands
            if " snapshot_metrics --lib -- --test-threads=1" in command
        ]
        self.assertEqual(len(snapshot_metrics), 1)
        self.assertIn("PLURX_EXPECT_TEST_COUNT=13", snapshot_metrics[0])
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
                'if [ "$RETAIN_STORE_UNITS" = true ]; then',
                '  echo "command=python3 -m validation.qualification_unit_continuation --compile-units store_contract" >> "$GITHUB_OUTPUT"',
                "  python3 -m validation.qualification_unit_continuation --compile-units store_contract 2>&1 | tee target/validation/cluster-store.log",
                "else",
                '  echo "command=make cluster-store-check" >> "$GITHUB_OUTPUT"',
                "  make cluster-store-check 2>&1 | tee target/validation/cluster-store.log",
                "fi",
            ],
        )

        store_receipt = store["Record the Store lane result"]
        self.assertEqual(workflow_step_scalar(store_receipt, "if"), "always()")
        self.assertIn(
            "LANE_RESULT: ${{ steps.store_contracts.outcome }}", store_receipt
        )
        self.assertIn('--result "$LANE_RESULT"', store_receipt)
        self.assertIn('LANE_COMMAND: ${{ steps.store_contracts.outputs.command }}', store_receipt)
        self.assertIn('--command "$LANE_COMMAND"', store_receipt)
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
                "    make hiqlite-vendor-clippy",
                '    if [ "$RETAIN_TOPOLOGY_UNITS" = true ]; then',
                "      python3 -m validation.qualification_unit_continuation --compile-units topology",
                "      make cluster-harness-nonunit-check",
                "    else",
                "      make cluster-harness-check",
                "    fi",
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
        self.assertIn("RETAIN_TOPOLOGY_UNITS: ${{ needs.scope.outputs.retain_topology_units }}", topology_receipt)
        self.assertIn(
            '          if [ "$RUN_CLUSTER_AUTH" = true ]; then\n'
            '            receipt+=(--command "make hiqlite-vendor-clippy")\n'
            '            if [ "$RETAIN_TOPOLOGY_UNITS" = true ]; then\n'
            '              receipt+=(--command "python3 -m validation.qualification_unit_continuation --compile-units topology")\n'
            '              receipt+=(--command "make cluster-harness-nonunit-check")\n'
            '            else\n'
            '              receipt+=(--command "make cluster-harness-check")\n'
            '            fi\n'
            '          fi\n',
            topology_receipt,
        )
        for command in (
            "make hiqlite-vendor-clippy",
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

    def test_full_unit_suite_has_dedicated_bounded_resident_capacity(self):
        job = workflow_job_blocks(".github/workflows/ci.yml")["check"]
        header = job.split("    steps:\n", 1)[0]
        self.assertIn(
            "    runs-on: [self-hosted, Linux, X64, lab, unit-memory-16g]\n",
            header,
        )
        self.assertIn(
            "      options: --cpus 3 --memory 16g --memory-swap 18g --pids-limit 4096\n",
            header,
        )
        self.assertIn('    env:\n      CARGO_BUILD_JOBS: "1"\n      RUST_TEST_THREADS: "2"\n', header)
        self.assertRegex(header, r"(?m)^    timeout-minutes: 120$")
        self.assertIn(
            FLEET_REGISTRY_EXPR + "/noirr/plurx-ci@sha256:"
            "3d84b711936233da699308d0301ac1e91f12e76c6b928967c41817e841f5e855",
            header,
        )
        self.assertEqual(
            workflow_step_scalar(workflow_step_blocks(job)[
                "Run format, Clippy, unit, and SQLite contracts"], "run"),
            "make ci-rust-gate",
        )
        self.assertNotIn("codegen-units", job)
        self.assertNotIn("CARGO_PROFILE_", job)
        self.assertNotIn("continue-on-error", job)
        carriers = [
            (name, labels) for name, labels in rostered_runners()
            if "unit-memory-16g" in labels
        ]
        # Only the existing capacity-one registration owns this resident budget.
        # A second carrier requires a separately reviewed host admission.
        self.assertEqual([name for name, _ in carriers], ["gha-media1-general-01"])
        for name, labels in carriers:
            self.assertIn("unit-memory-12g", labels, "retain existing consumers")
            self.assertFalse(
                {"general", "high-cpu", "ci-store", "ci-topology"} & labels, name
            )

    def test_web_layout_budget_covers_cold_build_and_all_browser_stages(self):
        job = workflow_job_blocks(".github/workflows/ci.yml")["web_layout"]
        self.assertRegex(job, r"(?m)^    timeout-minutes: 45$")
        steps = workflow_step_blocks(job)
        required = (
            ("Check every affected web layout", "make ui-check"),
            ("Run the playback-control reporter in a real browser", "make control-browser-check"),
            ("Test Cinema remote browser contracts", "make cinema-remote-browser-check"),
            ("Test grouped library rows in a real browser", "make library-rows-browser-check"),
            ("Test subtitle model setup and generated-caption completion in a real browser", "make subtitle-generation-browser-check"),
        )
        positions = []
        for name, command in required:
            self.assertEqual(workflow_step_scalar(steps[name], "run"), command)
            positions.append(list(steps).index(name))
        self.assertEqual(positions, sorted(positions))
        self.assertNotIn("continue-on-error", job)
        ui = read("Makefile").split("ui-check:", 1)[1].split(".PHONY:", 1)[0]
        self.assertIn("scripts/ui-baseline --self-host --check", ui)
        self.assertIn("python3 scripts/live-tv-browser --self-host --out target/live-tv-browser", ui)
        capture = read("scripts/ui-baseline")
        self.assertIn('p.add_argument("--timeout", type=int, default=45000,', capture)
        self.assertRegex(capture, r"(?m)^                attempts = 2$")

    def test_full_runtime_job_allowances_preserve_the_existing_suites(self):
        jobs = workflow_job_blocks(".github/workflows/ci.yml")
        for name, minutes in (("check", 120), ("cluster_store_legacy", 95), ("cluster_wal", 60)):
            with self.subTest(job=name):
                self.assertRegex(jobs[name], rf"(?m)^    timeout-minutes: {minutes}$")
                # The serial Store suite is not a compiler lane; the full unit
                # and WAL compilers bound Cargo concurrency on their queues.
                if name == "cluster_store_legacy":
                    self.assertNotIn("CARGO_BUILD_JOBS", jobs[name])
        unit = workflow_step_blocks(jobs["check"])
        self.assertEqual(workflow_step_scalar(
            unit["Run format, Clippy, unit, and SQLite contracts"], "run"), "make ci-rust-gate")
        store = workflow_step_blocks(jobs["cluster_store_legacy"])
        # The unretained branch still runs the complete Store suite; main's
        # retained-unit continuation branch is pinned by its own contract.
        self.assertIn(
            "make cluster-store-check 2>&1 | tee target/validation/cluster-store.log",
            "\n".join(workflow_step_literal(store["Run replicated Store contracts"], "run")),
        )
        wal = workflow_step_blocks(jobs["cluster_wal"])
        self.assertIn("make cluster-wal-check", "\n".join(workflow_step_literal(
            wal["Run exact Hiqlite and WAL recovery regressions"], "run")))
        self.assertIn("execution_mode != 'accelerated'", jobs["cluster_store_legacy"])
        makefile = read("Makefile")
        self.assertIn("--test store_contract -- --test-threads=1", makefile)
        self.assertIn("--features cluster-read-cost-validation,hiqlite-contract-tests", makefile)

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
        self.assertIn(
            "uses: https://github.com/docker/build-push-action@"
            "10e90e3645eae34f1e60eeb005ba3a3d33f178e8",
            build,
        )
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
        self.assertIn(
            "find crates vendor -type f -exec touch {} + \\\n"
            "    && mkdir -p /src/target-store-contract/debug/deps",
            dockerfile,
        )
        self.assertIn('test "$#" -eq 1', dockerfile)
        self.assertIn("rustc -Vv > /rustc-vv.txt", dockerfile)
        self.assertIn("FROM scratch AS store-contract-binary", dockerfile)

    def test_store_sharding_keeps_a_weekly_complete_unsharded_backstop(self):
        workflow = read(".github/workflows/cluster-store-backstop.yml")
        job = workflow_job_blocks(".github/workflows/cluster-store-backstop.yml")[
            "cluster-store-backstop"
        ]

        self.assertNotIn("schedule:", workflow)
        self.assertIn("workflow_dispatch:", workflow)
        self.assertIn("runs-on: [self-hosted, Linux, X64, lab, ci-store]", job)
        self.assertIn(
            "uses: https://github.com/dtolnay/rust-toolchain@"
            "4716b85f2fac3e324e64fa2810f6b5c3905760a5",
            job,
        )
        self.assertIn("lane: cluster-store-backstop", job)
        # No eligibility input any more: a self-hosted runner always gets the
        # bounded runner-local cache, and always enforces it on the way out.
        self.assertNotIn("persistent-eligible", job)
        self.assertIn("uses: ./.github/actions/cargo-cache-finalize", job)
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
        self.assertNotIn("persistent-eligible", package)
        self.assertIn(
            'run: scripts/ci-buildkit-prune "$BUILDER_NAME" 50', package
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
        self.assertIn(FLEET_REGISTRY_EXPR + "/noirr/android-build", workflow)
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
            "\n  package_smoke:", 1
        )[0]
        self.assertEqual(
            android_device.count(
                "uses: ./.github/actions/android-emulator-ready"
            ),
            2,
        )
        self.assertIn('node-version: "24"', android_device)
        self.assertIn("npm ci --prefix .github/actions/android-emulator-ready --ignore-scripts --omit=dev", android_device)
        provenance = read(".github/actions/android-emulator-ready/upstream.json")
        self.assertIn("4c44018e59b437e86cdfc41da381398f93ed8808", provenance)
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
        self.assertNotIn("persistent-eligible", store)
        self.assertIn("uses: ./.github/actions/cargo-cache-finalize", store)
        self.assertIn("uses: ./.github/actions/cargo-cache", topology)
        self.assertIn("lane: cluster-topology", topology)
        self.assertNotIn("persistent-eligible", topology)
        self.assertIn("uses: ./.github/actions/cargo-cache-finalize", topology)
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
        wal_run = workflow_step_blocks(wal)["Run exact Hiqlite and WAL recovery regressions"]
        self.assertEqual(workflow_step_literal(wal_run, "run"), [
            'if [ "$RETAIN_WAL_UNITS" = true ]; then',
            "  python3 -m validation.qualification_unit_continuation --compile-units wal",
            "else", "  make cluster-wal-check", "fi",
        ])
        self.assertIn("run: make cluster-daemon-check", daemon)
        self.assertIn("name: Verify the cluster fixture generator", daemon)
        self.assertIn("command -v ffmpeg", daemon)
        self.assertNotIn("spikes/hiqlite-m0/target", workflow)
        self.assertIn("name: cluster-topology-semantic", topology)
        self.assertIn(
            "path: target/validation/cluster-topology-semantic.json", topology
        )
        self.assertIn("if-no-files-found: error", topology)

    def test_transport_recovery_campaign_is_a_persistent_affected_linux_gate(self):
        workflow = read(".github/workflows/ci.yml")
        jobs = workflow_job_blocks(".github/workflows/ci.yml")
        contracts = jobs["cluster_transport_recovery_contracts"]
        voter = jobs["cluster_transport_recovery_voter"]
        learner = jobs["cluster_transport_recovery_learner"]
        recovery = jobs["cluster_transport_recovery"]
        contract_commands = make_dry_run_commands(
            "cluster-transport-recovery-contracts"
        )
        contract_run = workflow_step_blocks(contracts)["Run recovery schemas, validators, and focused regressions"]
        self.assertEqual(workflow_step_literal(contract_run, "run"), [
            "set -euo pipefail", "mkdir -p target/validation", "{",
            '  test "$(uname -s)" = Linux',
            '  if [ "$RETAIN_TRANSPORT_UNITS" = true ]; then',
            "    python3 -m validation.qualification_unit_continuation --compile-units transport",
            "  else", "    make cluster-transport-recovery-contracts", "  fi",
            "} 2>&1 | tee target/validation/cluster-transport-recovery-contracts.log",
        ])
        self.assertIn("PLURX_RETAINED_UNITS: ${{ needs.scope.outputs.retained_units }}", contract_run)
        receipt = workflow_step_blocks(recovery)["Record the transport recovery lane result"]
        self.assertIn("RETAIN_TRANSPORT_UNITS: ${{ needs.scope.outputs.retain_transport_units }}", receipt)
        self.assertEqual(workflow_step_literal(receipt, "run"), [
            'set -euo pipefail',
            'unit_command="make cluster-transport-recovery-contracts"',
            'if [ "$RETAIN_TRANSPORT_UNITS" = true ]; then',
            '  unit_command="python3 -m validation.qualification_unit_continuation --compile-units transport"',
            'fi',
            'python3 -m validation.ci_lane_receipt \\',
            '  --output target/validation/cluster-transport-recovery-receipt.json \\',
            '  --log target/validation/cluster-transport-recovery.log \\',
            '  --evidence target/validation/cluster-transport-recovery.json \\',
            '  --evidence-validator "$EVIDENCE_VALIDATOR" \\',
            '  --lane cluster-transport-recovery \\',
            '  --result "$LANE_RESULT" \\',
            '  --command "$unit_command" \\',
            '  --command "make cluster-transport-recovery-voter-check" \\',
            '  --command "make cluster-transport-recovery-learner-check" \\',
            '  --command "transport-recovery-assemble"',
        ])

        self.assertEqual(len(contract_commands), 7)
        self.assertEqual(contract_commands[0], 'test "$(uname -s)" = Linux')
        self.assertIn("PLURX_EXPECT_TEST_COUNT=76", contract_commands[1])
        self.assertIn("PLURX_EXPECT_TEST_NAMES=", contract_commands[1])
        self.assertIn("scripts/require-test-count", contract_commands[1])
        self.assertIn("transport_recovery --lib", contract_commands[1])
        exact_regressions = (
            "writer_exit_after_readiness_fails_the_recovery_promptly",
            "two_early_acknowledgements_cannot_hide_a_long_recovery_gap",
            "writer_must_cover_the_end_of_recovery",
            "whole_recovery_duration_cannot_exceed_the_absolute_deadline",
            "worst_transfer_duration_covers_the_complete_source_series",
            "artifact_publication_replaces_stale_bytes_without_exposing_a_partial_result",
            "owned_async_task_growth_has_no_resource_slack",
            "owned_async_task_guard_tracks_abort_safe_lifetime",
            "recovery_series_preserves_failed_retry_and_resets_after_completion",
            "outbound_retry_counts_only_when_openraft_issues_the_next_chunk",
            "recovery_series_counts_every_replacement_client_connection_attempt",
            "production_snapshot_executor_worker_is_counted_until_joined",
        )
        for regression in exact_regressions[:7]:
            self.assertIn("transport_recovery::tests::" + regression, contract_commands[1])
            self.assertTrue(all(regression not in command for command in contract_commands[2:]))
        for offset, regression in enumerate(exact_regressions[7:], start=2):
            self.assertIn(regression, contract_commands[offset])
            self.assertIn("scripts/require-test-count", contract_commands[offset])
            self.assertIn("--lib -- --exact", contract_commands[offset])
            self.assertIn("vendor/hiqlite/Cargo.toml", contract_commands[offset])

        voter_commands = make_dry_run_commands(
            "cluster-transport-recovery-voter-check"
        )
        learner_commands = make_dry_run_commands(
            "cluster-transport-recovery-learner-check"
        )
        smoke_commands = make_dry_run_commands("cluster-transport-recovery-smoke")
        full_commands = make_dry_run_commands("cluster-transport-recovery-check")
        self.assertIn("--role voter --cycles 20", voter_commands[1])
        self.assertIn("--role learner --cycles 20", learner_commands[1])
        self.assertIn("voter_status=0; learner_status=0", smoke_commands[1])
        self.assertIn("--cycles \"3\"", smoke_commands[1])
        self.assertIn("|| voter_status=$?", smoke_commands[1])
        self.assertIn("|| learner_status=$?", smoke_commands[1])
        self.assertIn("--role voter --cycles 20", full_commands[-1])
        self.assertIn("--role learner --cycles 20", full_commands[-1])
        self.assertIn("transport-recovery-assemble", full_commands[-1])
        self.assertIn("target/validation/cluster-transport-recovery.json", full_commands[-1])

        self.assertIn("cargo build --locked", voter_commands[1])
        self.assertIn("PLURX_BUILD_SHA=", voter_commands[1])
        external_binary_commands = make_dry_run_commands(
            "cluster-transport-recovery-voter-check", "RECOVERY_BINARY=/bin/true"
        )
        self.assertIn('test -x "/bin/true"', external_binary_commands[1])
        self.assertNotIn("cargo build", external_binary_commands[1])
        self.assertIn('option_env!("PLURX_BUILD_SHA")', read("crates/plurx-cluster-check/src/topology.rs"))
        self.assertIn(
            "qualification runner omitted its embedded PLURX_BUILD_SHA",
            read("crates/plurx-cluster-check/src/topology.rs"),
        )
        self.assertIn(
            'cargo:rerun-if-env-changed=PLURX_BUILD_SHA',
            read("crates/plurx-cluster-check/build.rs"),
        )

        for commands in (smoke_commands, full_commands):
            expanded = " ".join(commands)
            execution_ids = re.findall(
                r'--execution-id "(local-[^"]+)"', expanded
            )
            output_roots = re.findall(
                r'target/validation/transport-recovery/(local-[^/"]+)', expanded
            )
            self.assertGreaterEqual(len(execution_ids), 2)
            self.assertGreaterEqual(len(output_roots), 4)
            self.assertEqual(len(set((*execution_ids, *output_roots))), 1)

        for job in (contracts, voter, learner, recovery):
            self.assertIn("needs.scope.outputs.cluster_auth == 'true'", job)
            self.assertIn(
                "runs-on: [self-hosted, Linux, X64, lab, ci-topology]", job
            )
            self.assertIn("rust-toolchain@4716b85f2fac3e324e64fa2810f6b5c3905760a5", job)
            self.assertNotIn("persistent-eligible", job)
            self.assertIn("uses: ./.github/actions/cargo-cache-finalize", job)
        self.assertEqual(
            workflow_job_needs(voter),
            ("scope", "preflight", "cluster_transport_recovery_contracts"),
        )
        self.assertEqual(
            workflow_job_needs(learner),
            ("scope", "preflight", "cluster_transport_recovery_contracts"),
        )
        self.assertNotIn("cluster_transport_recovery_learner", voter)
        self.assertNotIn("cluster_transport_recovery_voter", learner)
        recovery_needs = workflow_job_needs(recovery)
        self.assertIn("cluster_transport_recovery_contracts", recovery_needs)
        self.assertIn("cluster_transport_recovery_voter", recovery_needs)
        self.assertIn("cluster_transport_recovery_learner", recovery_needs)
        self.assertIn("if: always()", recovery)
        self.assertIn("CONTRACTS_RESULT", recovery)
        self.assertIn("VOTER_RESULT", recovery)
        self.assertIn("LEARNER_RESULT", recovery)
        self.assertIn("test \"$VOTER_RESULT\" = success", recovery)
        self.assertIn("test \"$LEARNER_RESULT\" = success", recovery)
        self.assertIn("name: cluster-transport-recovery-voter", recovery)
        self.assertIn("name: cluster-transport-recovery-learner", recovery)
        self.assertNotIn("run-id:", recovery)
        self.assertIn("ci-${GITHUB_RUN_ID}-${GITHUB_RUN_ATTEMPT}", recovery)
        self.assertIn("make cluster-transport-recovery-voter-check", voter)
        self.assertIn("make cluster-transport-recovery-learner-check", learner)
        self.assertIn("--profile transport-recovery", voter)
        self.assertIn("--profile transport-recovery", learner)
        self.assertIn("RECOVERY_CARGO_PROFILE=transport-recovery", voter)
        self.assertIn("RECOVERY_CARGO_PROFILE=transport-recovery", learner)
        self.assertIn("role_budget=$(( 13800 - elapsed ))", voter)
        self.assertIn("role_budget=$(( 13800 - elapsed ))", learner)
        self.assertIn("voter-diagnostics", voter)
        self.assertIn("learner-diagnostics", learner)
        self.assertIn("retention-days: 14", voter)
        self.assertIn("retention-days: 14", learner)
        self.assertIn("lane: cluster-transport-recovery", recovery)
        self.assertIn("cluster-transport-recovery-receipt.json", recovery)
        self.assertIn("target/validation/cluster-transport-recovery.json", recovery)
        self.assertIn(
            "--evidence target/validation/cluster-transport-recovery.json", recovery
        )
        self.assertIn(
            "EVIDENCE_VALIDATOR: ${{ steps.cargo-cache.outputs.target-dir }}/transport-recovery/plurx-cluster-check",
            recovery,
        )
        cargo = read("Cargo.toml")
        profile = cargo.split("[profile.transport-recovery]", 1)[1].split("[", 1)[0]
        self.assertRegex(profile, r'(?m)^inherits = "dev"$')
        self.assertRegex(profile, r"(?m)^opt-level = 2$")
        self.assertRegex(profile, r"(?m)^debug = 1$")
        self.assertRegex(profile, r"(?m)^debug-assertions = true$")
        self.assertRegex(profile, r"(?m)^overflow-checks = true$")
        build_script = read("crates/plurx-cluster-check/build.rs")
        self.assertIn('env::var("PROFILE")', build_script)
        self.assertIn('env::var("OPT_LEVEL")', build_script)
        self.assertIn('env::var("DEBUG")', build_script)
        self.assertIn('--evidence-validator "$EVIDENCE_VALIDATOR"', recovery)
        self.assertIn("if-no-files-found: error", recovery)
        self.assertIn("steps.transport_recovery.outcome != 'success'", recovery)
        receipt_contract = runpy.run_path(
            str(ROOT / "validation/ci_lane_receipt.py")
        )
        self.assertIn(
            "cluster-transport-recovery",
            receipt_contract["LANES"],
        )
        self.assertEqual(
            receipt_contract["FIRST_WORKFLOW_RUN_ATTEMPT"],
            "1",
        )

        for aggregate_name in ("publish_main", "pr_gate"):
            aggregate = jobs[aggregate_name]
            self.assertIn(
                "cluster_transport_recovery", workflow_job_needs(aggregate)
            )
            self.assertIn("CLUSTER_TRANSPORT_RECOVERY_RESULT", aggregate)
        self.assertIn(
            '"cluster_transport_recovery":"${{ needs.cluster_transport_recovery.result }}"',
            jobs["pr_gate"],
        )
        qualification_contract = runpy.run_path(
            str(ROOT / "validation/qualification.py")
        )
        self.assertEqual(
            qualification_contract["FIRST_WORKFLOW_RUN_ATTEMPT"],
            "1",
        )

        with (ROOT / "validation/points.toml").open("rb") as handle:
            catalog = tomllib.load(handle)
        checks = {entry["id"]: entry for entry in catalog["checks"]}
        campaign = checks["cluster-transport-recovery"]
        self.assertEqual(campaign["command"], "make cluster-transport-recovery-check")
        self.assertEqual(campaign["profiles"], ["full", "nightly"])
        self.assertEqual(campaign["platforms"], ["linux"])
        self.assertEqual(campaign["timeout_seconds"], 28800)
        points = {entry["id"]: entry for entry in catalog["points"]}
        for point in ("cluster.auth", "cluster.membership", "cluster.operations"):
            self.assertIn("cluster-transport-recovery", points[point]["checks"])

    def test_main_preflight_runtime_is_job_local_bounded_and_precedes_receipts(self):
        preflight = workflow_job_blocks('.github/workflows/main-fast-lane.yml')['preflight']
        steps = workflow_step_blocks(preflight)
        bootstrap = '\n'.join(workflow_step_literal(steps['Install the job-local Python receipt runtime'], 'run'))
        cleanup = '\n'.join(workflow_step_literal(steps['Remove the exact job-local Python receipt runtime'], 'run'))
        self.assertIn('mktemp -d "$RUNNER_TEMP/plurx-python312.XXXXXXXX"', bootstrap)
        self.assertIn('curl -q --fail --location', bootstrap)
        self.assertIn("--proto '=https' --proto-redir '=https'", bootstrap)
        self.assertIn('--max-time 180 --max-filesize 34285590', bootstrap)
        self.assertIn('731af898886c5f821890dc901eca3c651cca8e51fa7308c159d12a1194aeac91', bootstrap)
        self.assertIn('sha256sum --check --status', bootstrap)
        self.assertIn('sys.version_info[:2] == (3, 12)', bootstrap)
        self.assertIn('sys.platform == "linux"', bootstrap)
        self.assertIn('platform.machine() == "x86_64"', bootstrap)
        self.assertNotIn('GITHUB_TOKEN', bootstrap)
        self.assertLess(preflight.index('Install the job-local Python receipt runtime'),
                        preflight.index('Restore attributable main Python successes'))
        self.assertLess(preflight.index('Publish preflight attempt-start journal'),
                        preflight.index('Audit corrective-history evidence'))
        self.assertIn('if: always()', steps['Remove the exact job-local Python receipt runtime'])
        self.assertIn('test "$(dirname "$PLURX_PYTHON_DIRECTORY")" = "$RUNNER_TEMP"', cleanup)
        self.assertIn('test ! -L "$PLURX_PYTHON_DIRECTORY"', cleanup)
        self.assertIn('rm -rf -- "$PLURX_PYTHON_DIRECTORY"', cleanup)
        self.assertIn('plurx-python312.????????', cleanup)

    def test_workflow_cancellation_preserves_qualification_and_python_receipts(self):
        workflow = read(".github/workflows/ci.yml")
        effort_workflow = read(".github/workflows/effort-ci.yml")

        for contract in (
            "github.event.pull_request.base.ref == 'main'",
            "startsWith(github.event.pull_request.head.ref, 'effort/')",
            "startsWith(github.event.pull_request.head.ref, 'integration/')",
            "endsWith(github.event.pull_request.head.ref, '-into-main')",
            "github.event.pull_request.head.ref != 'integration/-into-main'",
            "github.event_name == 'push' && github.ref == 'refs/heads/main'",
        ):
            self.assertIn(contract, workflow)
        self.assertIn("group: ci-${{ github.ref }}", workflow)
        self.assertNotIn("github.event.pull_request.number || github.ref", workflow)
        self.assertIn("cancel-in-progress: false", effort_workflow)
        self.assertNotIn("cancel-in-progress: true", effort_workflow)
        main_workflow = read(".github/workflows/main-fast-lane.yml")
        self.assertIn("cancel-in-progress: false", main_workflow)
        self.assertNotIn("cancel-in-progress: true", main_workflow)
        main_preflight = workflow_job_blocks(".github/workflows/main-fast-lane.yml")["preflight"]
        self.assertIn("validation.main_unit_receipts prepare", main_preflight)
        self.assertIn("validation.main_unit_receipts run --suite-dir tests/validation --suite-dir tests/operations", main_preflight)
        self.assertIn("if: always() && steps.receipts.outcome == 'success'", main_preflight)
        self.assertIn("name: ${{ steps.receipts.outputs.receipt_key }}-start-${{ github.run_id }}", main_preflight)
        self.assertIn("path: .main-python-unit-receipts/receipt.json", main_preflight)

        def cancels(
            event: str, *, ref: str = "", head: str = "", base: str = ""
        ) -> bool:
            promotion = base == "main" and (
                head.startswith("effort/")
                or (
                    head.startswith("integration/")
                    and head.endswith("-into-main")
                    and head != "integration/-into-main"
                )
            )
            return (event == "pull_request" and not promotion) or (
                event == "push" and ref == "refs/heads/main"
            )

        event_table = (
            ("effort task PR", "pull_request", "codex/task", "effort/project", "", True),
            ("ordinary main PR", "pull_request", "codex/task", "main", "", True),
            ("effort promotion", "pull_request", "effort/project", "main", "", False),
            ("integration promotion", "pull_request", "integration/project-into-main", "main", "", False),
            ("main push", "push", "", "", "refs/heads/main", True),
            ("release tag", "push", "", "", "refs/tags/v1.2.3", False),
        )
        for name, event, head, base, ref, expected in event_table:
            with self.subTest(name=name):
                self.assertEqual(
                    cancels(event, ref=ref, head=head, base=base), expected
                )

        pr_gate = workflow_job_blocks(".github/workflows/ci.yml")["pr_gate"]
        resolve_refs = workflow_step_blocks(pr_gate)[
            "Resolve the current qualification refs"
        ]
        self.assertIn("refs/plurx/current-head", resolve_refs)
        self.assertIn("refs/plurx/current-base", resolve_refs)
        self.assertIn("PLURX_CURRENT_HEAD_SHA", resolve_refs)
        self.assertIn("PLURX_CURRENT_BASE_SHA", resolve_refs)

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
            workflow.count(
                "uses: https://data.forgejo.org/forgejo/upload-artifact@"
                "16871d9e8cfcf27ff31822cac382bbb5450f1e1e"
            ),
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

    def test_fleet_registry_variable_always_carries_its_fallback(self):
        # A bare `${{ vars.FLEET_REGISTRY }}` expands to "" when the variable
        # is missing, which turns `docker login`, `docker/login-action` and the
        # image tag into Docker Hub references. Every use must carry the
        # fallback, and the fallback must not be a real registry.
        seen = 0
        for path in sorted((ROOT / ".github").rglob("*.yml")):
            text = path.read_text(encoding="utf-8")
            bare = re.findall(r"vars\.FLEET_REGISTRY(?!\s*\|\|)", text)
            self.assertEqual(bare, [], f"{path.relative_to(ROOT)} uses FLEET_REGISTRY without a fallback")
            seen += text.count(FLEET_REGISTRY_EXPR)
        self.assertGreaterEqual(seen, 20)
        self.assertIn(".invalid", FLEET_REGISTRY_EXPR)

    def test_no_workflow_pipes_git_show_into_an_early_exiting_grep(self):
        # `git show <big file> | grep -q` under `set -o pipefail` fails when
        # grep exits at its first match and git show is killed by SIGPIPE:
        # the v0.7.0 publication reported a present changelog heading as
        # missing. Read the blob into a file, then grep the file.
        early_exit = re.compile(r"git show[^|\n]*\|\s*grep\s+-[A-Za-z]*q")
        for path in sorted((ROOT / ".github").rglob("*.yml")):
            text = path.read_text(encoding="utf-8")
            self.assertEqual(
                early_exit.findall(text), [],
                f"{path.relative_to(ROOT)} pipes git show into grep -q",
            )

    def test_release_registry_and_weekly_readiness_match_ci(self):
        ci = read(".github/workflows/ci.yml")
        publisher = read(".github/workflows/publish-release.yml")
        unraid = read("deploy/unraid-plurx.xml")
        readiness = read(".github/workflows/release-readiness.yml")

        self.assertIn("uses: ./.github/workflows/publish-release.yml", ci)
        self.assertIn("REGISTRY_IMAGE: " + FLEET_REGISTRY_EXPR + "/noirr/plurxd", publisher)
        self.assertEqual(publisher.count("secrets.LOCAL_REGISTRY_TOKEN"), 4)
        self.assertEqual(
            publisher.count("buildkitd-config-inline: |"),
            4,
        )
        self.assertIn(
            "<Repository>forge.lan:3000/noirr/plurxd:main</Repository>",
            unraid,
        )
        self.assertIn(
            "<Registry>http://forge.lan:3000/noirr/-/packages/container/plurxd/main</Registry>",
            unraid,
        )
        # Weekly, from a green scheduled run: the cadence Paul chose on
        # 2026-09-23 (docs/RELEASING.md "The weekly release tag"). The tag
        # job itself is pinned in tests/operations/test_release_cut.py.
        self.assertIn('  schedule:\n    - cron: "0 6 * * 1"\n', readiness)
        self.assertIn("workflow_dispatch:", readiness)
        self.assertIn("run: make release-check", readiness)
        self.assertIn("fetch-depth: 0", readiness)

    def test_effort_web_lints_source_without_provisioning_behavior_browser(self):
        web = workflow_job_blocks(".github/workflows/effort-ci.yml")["web_static"]
        self.assertIn("run: make effort-web-static-check", web)
        self.assertNotIn("playwright", web.lower())
        self.assertNotIn("run: make web-check", web)

    def test_preflight_budgets_allow_full_history_and_contracts(self):
        for workflow in ("ci", "effort-ci", "main-fast-lane"):
            with self.subTest(workflow=workflow):
                preflight = workflow_job_blocks(
                    f".github/workflows/{workflow}.yml"
                )["preflight"]
                budget = 10 if workflow == "ci" else 15
                self.assertIn(f"timeout-minutes: {budget}", preflight)
                for command in (
                    "make history-check",
                    "make validation-lint",
                    "python3 -m validation.python_unit_receipts run"
                    if workflow == "effort-ci" else
                    "python3 -m validation.main_unit_receipts run"
                    if workflow == "main-fast-lane" else "make operations-check",
                ):
                    self.assertIn(command, preflight)

    def test_only_manual_main_dispatch_publishes_after_validation(self):
        workflow = read(".github/workflows/ci.yml")
        jobs = workflow_job_blocks(".github/workflows/ci.yml")
        publish = jobs["publish_main"]
        script = read("scripts/registry-push")

        self.assertIn("name: publish merged image (Forgejo registry)", publish)
        condition = publish.split("    if: >-\n", 1)[1].split("\n    runs-on:", 1)[0]
        self.assertEqual(
            " ".join(condition.split()),
            "always() && github.event_name == 'workflow_dispatch' && "
            "github.ref == 'refs/heads/main' && inputs.promotion_pr == '' && "
            "inputs.promotion_head_sha == '' && inputs.promotion_base_sha == '' && "
            "needs.scope.outputs.qualification != 'true'",
        )
        for field in ("promotion_pr", "promotion_head_sha", "promotion_base_sha"):
            self.assertIn(f"inputs.{field} == ''", condition)
        self.assertIn("needs.scope.outputs.qualification != 'true'", condition)
        triggers = workflow.split("on:\n", 1)[1].split("\njobs:", 1)[0]
        self.assertIn("workflow_dispatch:", triggers)
        self.assertNotIn("branches: [main]", triggers)
        self.assertNotIn("  schedule:", triggers)
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
            if path.relative_to(ROOT).as_posix() in GITHUB_ONLY_WORKFLOWS:
                continue  # GitHub honours permissions; the GitHub-only contract scopes them
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
        for name in ("workspace", "fuzz", "scheduled"):
            with self.subTest(best_effort_reporter=name):
                reporter = jobs[name].split("rustsec/audit-check@", 1)[1].split(
                    "\n      -", 1
                )[0]
                self.assertIn("continue-on-error: true", reporter)
        self.assertIn("run: cargo audit", jobs["workspace"])
        self.assertIn("run: cargo audit", jobs["fuzz"])
        self.assertIn(
            "run: cargo audit --file target/rust-audit/Cargo.lock", scheduled
        )

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
        apple = local("macOS", "ARM64", "lab", "apple", "xcode-27.1")
        ci_store = local("Linux", "X64", "lab", "ci-store")
        ci_topology = local("Linux", "X64", "lab", "ci-topology")
        release_general = "    runs-on: [self-hosted, Linux, X64, lab, general]"
        release_high_cpu = (
            "    runs-on: [self-hosted, Linux, X64, lab, general, high-cpu]"
        )

        for path in (
            ".github/workflows/ci.yml",
            ".github/workflows/cluster-store-backstop.yml",
            ".github/workflows/coverage.yml",
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
                elif path == ".github/workflows/ci.yml" and name == "check":
                    expected = local("Linux", "X64", "lab", "unit-memory-16g")
                elif path == ".github/workflows/ci.yml" and name == "cluster_daemon":
                    # The pinned private CI image supplies shipped FFmpeg 8;
                    # host trust labels remain unchanged, without a FFmpeg-6
                    # installation requirement on these image-owned jobs.
                    expected = high_cpu
                elif path == ".github/workflows/ci.yml" and name in {
                    "web_layout", "vod_web"
                }:
                    expected = general
                elif path == ".github/workflows/coverage.yml" and name == "coverage":
                    expected = local("Linux", "X64", "lab", "ci-coverage")
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
                elif path == ".github/workflows/ci.yml" and name in {
                    "cluster_topology",
                    "cluster_transport_recovery_contracts",
                    "cluster_transport_recovery_voter",
                    "cluster_transport_recovery_learner",
                    "cluster_transport_recovery",
                }:
                    expected = ci_topology
                elif path == ".github/workflows/ci.yml" and name == "cluster_wal":
                    expected = local("Linux", "X64", "lab", "baremetal", "ci-topology")
                elif (
                    path in {
                        ".github/workflows/ci.yml",
                        ".github/workflows/effort-ci.yml",
                    }
                    and name == "rust_compile"
                ):
                    expected = local("Linux", "X64", "lab", "baremetal", "ci-topology")
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
                if name == "image":
                    # Each architecture builds natively on its own runner:
                    # never arm64 under QEMU on an X64 host (v0.7.0, #1163).
                    self.assertEqual(
                        "    runs-on: ${{ fromJSON(matrix.runs_on) }}",
                        runs_on.group(0),
                        f"{path}:{name}",
                    )
                    self.assertEqual(
                        {
                            ("self-hosted", "Linux", "X64", "lab", "general", "high-cpu"),
                            ("self-hosted", "Linux", "ARM64", "lab", "release-arm64"),
                        },
                        set(self_hosted_label_sets(block)),
                        f"{path}:{name} must build each architecture on its own runner",
                    )
                    continue
                expected = release_general if name == "resolve" else release_high_cpu
                self.assertEqual(expected, runs_on.group(0), f"{path}:{name}")

        for name, block in workflow_job_blocks(
            ".github/workflows/rust-audit.yml"
        ).items():
            self.assertIn(
                "uses: https://github.com/dtolnay/rust-toolchain@"
                "4716b85f2fac3e324e64fa2810f6b5c3905760a5",
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
            if path in GITHUB_ONLY_WORKFLOWS:
                continue  # GitHub-hosted by design; see the GitHub-only contract
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
        # matrix on 2026-09-02 after `gha-lab2-android-01` lost `ci-store` for
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
            if path.relative_to(ROOT).as_posix() in GITHUB_ONLY_WORKFLOWS:
                continue  # GitHub-hosted by design; see the GitHub-only contract
            self.assertNotRegex(workflow, r"(?m)^ +runs-on: (?:ubuntu|macos)-")

    def test_github_only_workflows_never_reach_the_fleet(self):
        # Forgejo runs every file in .github/workflows, and the GitHub mirror
        # is public. A GitHub-only workflow is safe on both sides only while
        # (1) every job's whole condition is the github.com gate, which Forgejo
        # evaluates on the server and skips without a runner, (2) nothing in
        # it can select a self-hosted runner or a container, (3) it runs only
        # local actions and GitHub's two pinned artifact actions, (4) nothing
        # but a tag push or a dispatch can start it, so fork code never runs,
        # and (5) only the one job that builds nothing holds a write token.
        # Every check is an exact match: a gate that merely contains the
        # condition (`... || true`) or a permission set that merely starts
        # with the allowed scope must fail.
        gate = "github.server_url == 'https://github.com'"
        artifact_action = re.compile(r"actions/(?:upload|download)-artifact@[0-9a-f]{40}")
        for path in sorted(GITHUB_ONLY_WORKFLOWS):
            with self.subTest(path=path):
                workflow = read(path)
                code = "\n".join(
                    line for line in workflow.splitlines()
                    if not line.lstrip().startswith("#")
                ) + "\n"
                on = re.search(r"(?m)^on:\n((?:(?:  .*)?\n)*)", code)
                self.assertIsNotNone(on, path)
                triggers = re.findall(r"(?m)^  ([a-z_]+):", on.group(1))
                qualification = path == ".github/workflows/desktop-qualification.yml"
                if qualification:
                    self.assertEqual(["workflow_dispatch", "workflow_call"], triggers, path)
                else:
                    self.assertEqual(["push", "workflow_dispatch"], triggers, path)
                    push = on.group(1).split("  workflow_dispatch:", 1)[0]
                    self.assertEqual(["tags"], re.findall(r"(?m)^    ([a-z_]+):", push), path)
                self.assertEqual(
                    ["  contents: read\n  actions: read\n"],
                    re.findall(r"(?m)^permissions:\n((?:  .*\n)*)", code),
                    path,
                )
                if qualification:
                    self.assertNotRegex(code, r"(?m)^\s+(?:environment|secrets):")
                    self.assertNotIn("secrets.", code)
                for forbidden in ("self-hosted", "pull_request", "workflow_run"):
                    self.assertFalse(forbidden in code, f"{path} names {forbidden}")
                self.assertIsNone(re.search(r"(?m)^\s+(?:container|services):", code), path)
                for action in re.findall(r"(?m)^\s*(?:-\s+)?uses:\s+(\S+)", code):
                    self.assertTrue(
                        action.startswith("./.github/actions/")
                        or artifact_action.fullmatch(action)
                        or (path == ".github/workflows/github-release.yml" and action == "./.github/workflows/desktop-qualification.yml"),
                        f"{path} uses {action}",
                    )
                jobs = workflow_job_blocks(path)
                self.assertTrue(jobs)
                writers = []
                for name, block in jobs.items():
                    conditions = re.findall(r"(?m)^    if: (.+)$", block)
                    # Production metadata signing must be skipped entirely on
                    # dry runs so fixture qualification never enters a protected
                    # environment or requests owner signing credentials.
                    expected_gate = gate
                    if path == ".github/workflows/github-release.yml":
                        if name == "desktop_signing":
                            expected_gate += " && needs.resolve.outputs.mode == 'release' && needs.resolve.outputs.publish_updates == 'true'"
                        elif name == "publish":
                            expected_gate += " && !cancelled() && !failure() && needs.resolve.outputs.mode == 'release'"
                        elif name == "macos_signing":
                            expected_gate += " && needs.resolve.outputs.mode == 'release'"
                        elif name == "desktop_metadata":
                            expected_gate += " && !cancelled() && !failure() && needs.resolve.outputs.mode != 'qualification'"
                        elif name == "desktop_qualification":
                            expected_gate += " && needs.resolve.outputs.mode == 'qualification'"
                        elif name != "resolve":
                            self.assertIn(name, {"linux", "macos_source_parser", "macos_build", "macos", "windows", "windows_smoke", "desktop_metadata"})
                            expected_gate += " && needs.resolve.outputs.mode != 'qualification'"
                    self.assertEqual([expected_gate], conditions, f"{path}:{name}")
                    if path == ".github/workflows/github-release.yml" and name == "desktop_qualification":
                        self.assertIn("    needs: resolve\n", block)
                        self.assertIn("    uses: ./.github/workflows/desktop-qualification.yml\n", block)
                        self.assertIn("      candidate_sha: ${{ github.sha }}\n", block)
                        self.assertIn("      run_local_fixture_updates: true\n", block)
                        self.assertNotRegex(block, r"(?m)^    (?:permissions|runs-on|steps|environment|secrets):")
                        continue
                    self.assertIn("\n    timeout-minutes:", block, f"{path}:{name}")
                    runs_on = re.search(r"(?m)^    runs-on: (.+)$", block).group(1)
                    if runs_on == "${{ matrix.runner }}":
                        runners = set(re.findall(r"(?m)^ +runner: (\S+)$", block))
                    else:
                        runners = {runs_on}
                    self.assertTrue(runners, f"{path}:{name}")
                    self.assertLessEqual(runners, GITHUB_HOSTED_RUNNERS, f"{path}:{name}")
                    grants = re.findall(r"(?m)^    permissions:(.*)\n((?:      .*\n)*)", block)
                    if grants:
                        self.assertFalse(qualification, f"{path}:{name} may not hold job permission overrides")
                        self.assertEqual([("", "      contents: write\n")], grants, f"{path}:{name}")
                        writers.append(name)
                        # The writer runs nothing it was handed: no build, and
                        # nothing unpacked or executed from the artifacts.
                        for builder in (
                            "cargo", "./.github/actions/", "scripts/", "docker run",
                            "tar -x", "tar x", "unzip", "Expand-Archive", "dpkg",
                            "rpm -", "chmod", "bash assets", "sh assets", "./assets",
                        ):
                            self.assertNotIn(builder, block, f"{path}:{name} writes and runs {builder}")
                self.assertLessEqual(len(writers), 1, f"{path} has several writing jobs")

    def test_github_release_packages_what_the_workflow_publishes(self):
        workflow = read(".github/workflows/github-release.yml")
        self.assertIn('tags: ["v*.*.*"]', workflow)
        # The release is the tag's source, validated the way publish-release.yml
        # validates it, built with the stamp `plurxd --version` reports.
        self.assertIn('[[ "$tag" =~ ^v[0-9]+\\.[0-9]+\\.[0-9]+$ ]]', workflow)
        self.assertIn('test "$(git cat-file -t "refs/tags/$tag")" = tag', workflow)
        self.assertIn('grep -Fq "## [$version]" CHANGELOG.md', workflow)
        for key in ("PLURX_BUILD_REF", "PLURX_BUILD_SHA", "SOURCE_DATE_EPOCH"):
            self.assertGreaterEqual(workflow.count(f"      {key}: ${{{{ needs.resolve.outputs."), 3, key)
        # One packager per format, each pinned by digest or checksum.
        self.assertIn("scripts/package-linux --arch", workflow)
        self.assertIn("CARGO_TARGET_DIR=\"$PWD/release-source/target\" \\\n          PLURX_PKG_SOURCE_ROOT=\"$PWD/release-source\" \\\n            scripts/package-windows", workflow)
        self.assertIn("compile-workspace: \"false\"", workflow)
        self.assertRegex(workflow, r"BUILD_IMAGE: almalinux:8@sha256:[0-9a-f]{64}")
        self.assertEqual(len(re.findall(r"(?m)^ +rustup_sha256: [0-9a-f]{64}$", workflow)), 2)
        self.assertEqual(len(re.findall(r"(?m)^ +nfpm_sha256: [0-9a-f]{64}$", workflow)), 2)
        # The finished release holds exactly the six artifacts plus checksums,
        # and a back-filled older tag never becomes "latest".
        for asset in (
            "plurxd_${VERSION}-1_amd64.deb", "plurxd_${VERSION}-1_arm64.deb",
            "plurxd-${VERSION}-1.x86_64.rpm", "plurxd-${VERSION}-1.aarch64.rpm",
            "plurxd-${VERSION}-macos-universal.tar.gz",
            "plurxd-${VERSION}-windows-x86_64.zip",
        ):
            self.assertIn(asset, workflow)
        self.assertIn("SHA256SUMS", workflow)
        self.assertIn('--notes-file notes.md --latest="$latest"', workflow)
        packaging = read("deploy/packaging/nfpm.yaml")
        self.assertIn("dst: /usr/lib/systemd/system/plurxd.service", packaging)
        self.assertIn("dst: /usr/lib/plurx/plurxd.dwp", packaging)
        self.assertNotIn("dst: /var/lib/plurx", packaging)
        self.assertIn("ffmpeg (>= 7:5.1)", packaging)
        self.assertIn('install -d -o plurx -g plurx -m 0750 "$state"', read("deploy/packaging/postinstall"))
        self.assertIn("ExecStart=/usr/local/bin/plurxd run", read("deploy/plurxd.service"))
        windows = read(".github/actions/windows-cross/action.yml")
        self.assertIn("if: inputs.compile-workspace != 'false'", windows)
        self.assertIn('default: "true"', windows)

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

    def test_desktop_libraries_are_build_dependencies_not_server_runtime(self):
        for path in (".github/workflows/ci.yml", ".github/workflows/effort-ci.yml",
                     ".github/workflows/main-fast-lane.yml"):
            with self.subTest(workflow=path):
                job = workflow_job_blocks(path)["rust_compile"]
                self.assertIn("container: ubuntu:24.04", job)
                steps = workflow_step_blocks(job)
                setup = steps["Install Rust gate prerequisites"]
                self.assertIn("libgtk-4-dev libdbus-1-dev", setup)
                self.assertLess(job.index(setup), job.index("make effort-rust-check"))
                restore = steps["Restore persistent runner workspace ownership"]
                self.assertIn("if: always()", restore)
                self.assertIn('chown -R "$owner" "$GITHUB_WORKSPACE"', restore)
        dockerfile = read("Dockerfile")
        ci_stage = dockerfile.split("FROM runtime-assets AS ci", 1)[1].split(
            "FROM runtime-assets AS runtime", 1)[0]
        self.assertIn("libgtk-4-dev libdbus-1-dev", ci_stage)
        self.assertNotIn("libgtk-4-dev", dockerfile.split("FROM runtime-assets AS ci", 1)[0])
        self.assertNotIn("libgtk-4-dev", dockerfile.split("FROM runtime-assets AS runtime", 1)[1])
        self.assertIn("cargo build --locked --release -p plurxd", dockerfile)
        unit = workflow_job_blocks(".github/workflows/ci.yml")["check"]
        self.assertIn('desktop: "true"', unit)
        self.assertLess(unit.index("uses: ./.github/actions/rust-native-deps"),
                        unit.index("run: make ci-rust-gate"))
        adapter = read(".github/actions/rust-native-deps/action.yml")
        self.assertIn("if: inputs.desktop == 'true'", adapter)
        self.assertIn("pkg-config --exists 'gtk4 >= 4.0' dbus-1", adapter)

    def test_container_rust_jobs_supply_openssl_before_compilation(self):
        ci_stage = read("Dockerfile").split("FROM runtime-assets AS ci", 1)[1].split(
            "FROM runtime-assets AS runtime", 1
        )[0]
        self.assertIn("libssl-dev", ci_stage)
        jobs = workflow_job_blocks(".github/workflows/ci.yml")
        checked = set()
        for name, block in jobs.items():
            if "/noirr/plurx-ci@sha256:" not in block:
                continue
            with self.subTest(job=name):
                dependency = "uses: ./.github/actions/rust-native-deps"
                self.assertIn(dependency, block)
                # Provision before the first compiler-capable local action.
                self.assertLess(block.index(dependency), block.index("uses: ./.github/actions/ffmpeg"))
                checked.add(name)
        self.assertEqual(checked, {"check", "cluster_daemon", "web_layout", "vod_web"})
        action = read(".github/actions/rust-native-deps/action.yml")
        self.assertIn("pkg-config --exists openssl", action)
        self.assertIn("#include <openssl/ssl.h>", action)
        self.assertIn("OPENSSL_init_ssl", action)
        self.assertIn('"$probe/openssl"', action)

    def test_m5_shipped_ffmpeg_jobs_use_one_digest_and_keep_burst_coverage(self):
        dockerfile = read("Dockerfile")
        self.assertIn("FROM runtime-assets AS ci", dockerfile)
        ci_stage = dockerfile.split("FROM runtime-assets AS ci", 1)[1].split(
            "FROM runtime-assets AS runtime", 1
        )[0]
        self.assertIn("COPY LICENSE NOTICE THIRD-PARTY-NOTICES.md /usr/share/doc/plurx/", ci_stage)
        self.assertIn("COPY licenses/ /usr/share/doc/plurx/licenses/", ci_stage)
        self.assertIn("PLAYWRIGHT_BROWSERS_PATH=/opt/playwright-browsers", dockerfile)
        self.assertLess(
            dockerfile.index("PLAYWRIGHT_BROWSERS_PATH=/opt/playwright-browsers"),
            dockerfile.index("python3 -m playwright install --with-deps chromium"),
        )
        self.assertIn("PLURX_FFMPEG=/usr/lib/jellyfin-ffmpeg/ffmpeg", dockerfile)
        self.assertIn("/usr/local/bin/ffmpeg", dockerfile)

        jobs = workflow_job_blocks(".github/workflows/ci.yml")
        image = None
        self.assertNotIn("coverage", jobs)
        for name in ("check", "cluster_daemon", "web_layout", "vod_web"):
            with self.subTest(job=name):
                block = jobs[name]
                self.assertNotIn("ffmpeg-6", block)
                self.assertIn('major: "8"', block)
                self.assertIn(
                    "binary: /usr/lib/jellyfin-ffmpeg/ffmpeg", block
                )
                steps = workflow_step_blocks(block)
                capture = steps["Capture mounted runner workspace owner"]
                restore = steps["Restore persistent runner workspace ownership"]
                self.assertIn('dirname -- "$GITHUB_WORKSPACE"', capture)
                self.assertIn('echo "HOST_WORKSPACE_OWNER=$owner"', capture)
                self.assertIn('"${owner%%:*}" = 0', capture)
                self.assertIn('"${owner##*:}" = 0', capture)
                self.assertIn('chown -R "$HOST_WORKSPACE_OWNER"', restore)
                self.assertNotIn('stat -c', restore)
                self.assertIn("    container:\n", block)
                match = re.search(r"(?m)^      image: (.+)$", block)
                self.assertIsNotNone(match)
                pinned = match.group(1)
                self.assertRegex(
                    pinned,
                    r"^\$\{\{ vars\.FLEET_REGISTRY \|\| 'fleet-registry\.unset\.invalid' \}\}"
                    r"/noirr/plurx-ci@sha256:[0-9a-f]{64}$",
                )
                if image is None:
                    image = pinned
                self.assertEqual(image, pinned)
                self.assertIn("      credentials:\n        username: noirr\n", block)
                self.assertIn(
                    "        password: ${{ secrets.CI_REGISTRY_PULL_TOKEN }}", block
                )
                self.assertNotIn("secrets.LOCAL_REGISTRY_TOKEN", block.split("    steps:", 1)[0])

        fast = workflow_job_blocks(".github/workflows/main-fast-lane.yml")[
            "rust_compile"
        ]
        self.assertIn("container: ubuntu:24.04", fast)
        self.assertIn('major: "6"', fast)
        self.assertIn("make effort-rust-check", fast)
        self.assertNotIn("make unit", fast)
        coverage = workflow_job_blocks(".github/workflows/coverage.yml")["coverage"]
        self.assertIn("image: ubuntu:24.04", coverage)
        self.assertIn('major: "6"', coverage)
        self.assertNotIn("plurx-ci@sha256:", coverage)
        self.assertIn("cargo llvm-cov --workspace --locked --exclude plurx-cluster-check", coverage)
        action = read(".github/actions/ffmpeg/action.yml")
        self.assertIn('if [ -n "$WANT_BINARY" ]', action)

    def test_ci_flake_ledger_records_real_job_outcomes_and_durations(self):
        script = ROOT / "scripts/ci-flake-report"
        subprocess.run([str(script), "--help"], check=True, stdout=subprocess.PIPE)
        reporter = script.read_text(encoding="utf-8")
        self.assertIn('default="http://forge.lan:3000/api/v1"', reporter)
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

    def test_container_restore_smoke_drills_backup_and_offline_restore(self):
        # K-01 M4: the packaged image backs up a running instance through the
        # operator CLI, verifies and restores the artefact offline into an
        # empty second volume, and the restored instance keeps its identity,
        # users, tokens and libraries. The container lane runs it after the
        # stop/start smoke, and `make container-smoke` runs both.
        drill = read("scripts/container-restore-smoke")
        subprocess.run(
            ["sh", "-n", str(ROOT / "scripts/container-restore-smoke")], check=True
        )
        self.assertIn("trap cleanup EXIT HUP INT TERM", drill)
        self.assertIn("--publish 127.0.0.1:0:32400", drill)
        self.assertIn("--user 0:0", drill)
        self.assertIn("plurxd cluster backup", drill)
        self.assertIn("--token-file /tmp/restore-smoke.token", drill)
        self.assertIn("umask 077", drill)
        self.assertNotIn("--token ", drill)
        self.assertNotIn("docker exec -e", drill)
        self.assertNotIn("Bearer $", drill)
        self.assertIn('restore --verify --archive "/backups/$archive"', drill)
        self.assertIn(
            'restore --archive "/backups/$archive" --data-dir /var/lib/plurx', drill
        )
        self.assertIn('docker stop --time 10 "$source_name"', drill)
        self.assertIn("a second restore over the restored data directory was accepted", drill)
        self.assertIn('test "$instance_source" = "$instance_restored"', drill)
        self.assertIn('"$restored_base/api/v1/auth/login"', drill)
        self.assertIn('"$restored_base/api/v1/libraries"', drill)
        self.assertNotRegex(drill, r"curl [^\n]+\| grep -q")

        makefile = read("Makefile")
        target = makefile.split("container-smoke: docker", 1)[1].split("\n\n", 1)[0]
        self.assertIn("@scripts/container-smoke plurx/plurxd:latest", target)
        self.assertIn("@scripts/container-restore-smoke plurx/plurxd:latest", target)
        workflow = read(".github/workflows/ci.yml")
        package = workflow.split("  package_smoke:", 1)[1].split("\n  publish:", 1)[0]
        self.assertLess(
            package.index('scripts/container-smoke "$IMAGE"'),
            package.index('run: scripts/container-restore-smoke "$IMAGE"'),
        )

    def test_container_restore_smoke_keeps_secrets_off_every_command_line(self):
        # K-01 M4 review of #542: the drill's passwords and bearer tokens must
        # not appear in any process's argv (curl, the host `docker` client, or a
        # `docker exec -e` environment, which is argv to the client and is
        # recorded on the exec instance). Run the real script against recording
        # `docker` and `curl` stand-ins that emulate just enough of the image
        # and the API, and inspect every argv they were given.
        admin_token = "SECRET-admin-token-4b1d"
        tokens = {
            "admin": admin_token,
            "viewer": "SECRET-viewer-token-77c2",
            "relogin": "SECRET-relogin-token-90ae",
        }
        stub = textwrap.dedent(
            f"""\
            #!{sys.executable}
            import json, os, pathlib, sys
            state = pathlib.Path(os.environ["RESTORE_SMOKE_STUB_STATE"])
            name = pathlib.Path(sys.argv[0]).name
            args = sys.argv[1:]
            with open(state / "argv.jsonl", "a") as log:
                log.write(json.dumps([name, *args]) + "\\n")
            TOKENS = {json.dumps(tokens)}

            def docker():
                verb = args[0]
                if verb == "port":
                    print("127.0.0.1:4" + ("1" if "source" in args[1] else "2") + "000")
                elif verb == "exec" and "-i" in args:
                    (state / "exec-stdin").write_bytes(sys.stdin.buffer.read())
                elif verb == "exec" and "cluster" in args and "backup" in args:
                    delivered = (state / "exec-stdin").read_text().strip()
                    if "--token-file" not in args or delivered != TOKENS["admin"]:
                        sys.exit(2)
                    print('{{"status": "ok", "path": '
                          '"/var/lib/plurx/backups/plurx-backup-20260926T000000Z-stub",'
                          ' "manifest": {{"cluster_id": "stub"}}}}')
                elif verb == "run" and "--data-dir" in args:
                    marker = state / "restored"
                    if marker.exists():
                        sys.exit(1)
                    marker.touch()

            def curl():
                headers, body, url, it = [], None, None, iter(args)
                for arg in it:
                    if arg == "-H":
                        value = next(it)
                        if value.startswith("@"):
                            headers += pathlib.Path(value[1:]).read_text().splitlines()
                        else:
                            headers.append(value)
                    elif arg in ("--data", "--data-binary", "-d"):
                        value = next(it)
                        body = (pathlib.Path(value[1:]).read_text()
                                if value.startswith("@") else value)
                    elif not arg.startswith("-"):
                        url = arg
                path = "/" + url.split("/", 3)[3]
                bearer = {{h.split("Bearer ", 1)[1].strip() for h in headers
                          if h.lower().startswith("authorization: bearer ")}}
                admin = {{TOKENS["admin"], TOKENS["relogin"]}}
                if path == "/readyz":
                    print("ok")
                elif path == "/api/v1/server":
                    print('{{"instance_id": "stub-instance"}}')
                elif path == "/api/v1/setup":
                    print(json.dumps({{"token": TOKENS["admin"]}}))
                elif path == "/api/v1/auth/login":
                    user = json.loads(body)
                    if not user.get("password"):
                        sys.exit(22)
                    key = "viewer" if user["username"] == "restore-viewer" else "relogin"
                    print(json.dumps({{"token": TOKENS[key]}}))
                elif path in ("/api/v1/users", "/api/v1/libraries"):
                    if not bearer & admin:
                        sys.exit(22)
                    print('[{{"name": "Restore Smoke Movies"}}]' if body is None else "{{}}")
                else:
                    sys.exit(22)

            docker() if name == "docker" else curl()
            """
        )
        with tempfile.TemporaryDirectory() as temporary:
            temporary_path = Path(temporary)
            stubs = temporary_path / "bin"
            state = temporary_path / "state"
            scratch = temporary_path / "tmp"
            for directory in (stubs, state, scratch):
                directory.mkdir()
            for name in ("docker", "curl"):
                (stubs / name).write_text(stub)
                (stubs / name).chmod(0o755)
            result = subprocess.run(
                ["sh", str(ROOT / "scripts/container-restore-smoke"), "stub-image"],
                cwd=ROOT,
                env=dict(
                    os.environ,
                    PATH=f"{stubs}:{os.environ['PATH']}",
                    TMPDIR=str(scratch),
                    RESTORE_SMOKE_STUB_STATE=str(state),
                ),
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                timeout=60,
            )
            invocations = [
                json.loads(line)
                for line in (state / "argv.jsonl").read_text().splitlines()
            ]
            secrets = [*tokens.values(), "restore-smoke-admin-", "restore-smoke-viewer-"]
            leaks = [
                " ".join(argv)
                for argv in invocations
                if any(secret in arg for arg in argv for secret in secrets)
            ]
            self.assertEqual([], leaks, "a secret reached a command line")
            self.assertEqual(0, result.returncode, result.stderr)
            self.assertIn("container restore smoke passed", result.stdout)
            # The stand-ins only answer authenticated calls when the bearer
            # arrived through a header file, and the in-container token file
            # received exactly the admin token over stdin.
            self.assertEqual(admin_token, (state / "exec-stdin").read_text().strip())
            self.assertTrue(
                any(argv[:2] == ["docker", "exec"] and "backup" in argv for argv in invocations)
            )
            self.assertEqual([], list(scratch.iterdir()), "the drill left scratch state")

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
