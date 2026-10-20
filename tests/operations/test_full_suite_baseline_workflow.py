"""Execute diagnostic controls from the workflow with hostile/failed inputs."""
from __future__ import annotations

import os
from pathlib import Path
import re
import subprocess
import tempfile
import textwrap
import unittest
from unittest import mock

from validation import baseline_commands

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ROOT / ".github/workflows/ci.yml"


def jobs():
    source = WORKFLOW.read_text().split("\njobs:\n", 1)[1]
    starts = list(re.finditer(r"(?m)^  ([a-zA-Z0-9_-]+):\n", source))
    return {m[1]: source[m.start(): starts[i + 1].start() if i + 1 < len(starts) else len(source)]
            for i, m in enumerate(starts)}


def step(job, name):
    starts = list(re.finditer(r"(?m)^      - (?:name|uses): (.+)\n", job))
    for i, match in enumerate(starts):
        if match[1] == name:
            return job[match.start(): starts[i + 1].start() if i + 1 < len(starts) else len(job)]
    raise AssertionError(f"Missing workflow step {name}")


def script(block, diagnostic="true"):
    body = block.split("        run: |\n", 1)[1]
    return textwrap.dedent(body).replace("${{ inputs.diagnostic_baseline }}", diagnostic)


def condition(job):
    lines = job.splitlines()
    i = next(i for i, line in enumerate(lines) if line.startswith("    if: "))
    first = lines[i].split("if: ", 1)[1]
    if first != ">-":
        return first
    values = []
    for line in lines[i + 1:]:
        if not line.startswith("      "):
            break
        values.append(line.strip())
    return " ".join(values)


def selected(job, **overrides):
    values = {
        "inputs.diagnostic_baseline": "true", "inputs.promotion_pr": "",
        "inputs.promotion_head_sha": "", "inputs.promotion_base_sha": "",
        "github.event_name": "workflow_dispatch", "github.ref": "refs/heads/main",
        "needs.scope.result": "success", "needs.preflight.result": "failure",
        "needs.web_layout.result": "success",
        "needs.cluster_transport_recovery_contracts.result": "success",
        "needs.scope.outputs.execution_mode": "legacy",
        "needs.scope.outputs.qualification": "false",
    }
    values.update(overrides)
    def value(match):
        name = match[0]
        if name not in values:
            if ".outputs.retain_" in name:
                return repr("")
            if ".outputs." in name:
                return repr("true")
            raise AssertionError(f"Unhandled workflow condition input {name}")
        return repr(values[name])
    expression = re.sub(r"\b(?:inputs|needs|github)\.[A-Za-z0-9_.]+", value, condition(job))
    expression = expression.replace("always()", "True").replace("&&", " and ").replace("||", " or ")
    return bool(eval(expression, {"__builtins__": {}, "startsWith": lambda a, b: a.startswith(b)}))


class FullSuiteBaselineWorkflowCase(unittest.TestCase):
    def run_shell(self, body, environment=None, executables=None):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            tools = root / "bin"
            tools.mkdir()
            for name, content in (executables or {}).items():
                tool = tools / name
                tool.write_text("#!/bin/sh\n" + content)
                tool.chmod(0o755)
            env = {**os.environ, "PATH": str(tools) + os.pathsep + os.environ["PATH"],
                   "GITHUB_OUTPUT": str(root / "output"), "TRACE": str(root / "trace"),
                   **(environment or {})}
            result = subprocess.run(["bash", "--noprofile", "--norc", "-e", "-o", "pipefail", "-c", body],
                                    cwd=root, env=env, text=True, capture_output=True)
            output = (root / "output").read_text() if (root / "output").exists() else ""
            trace = (root / "trace").read_text() if (root / "trace").exists() else ""
            return result, output, trace

    def binding(self, **overrides):
        sha = "a" * 40
        env = dict(DIAGNOSTIC_BASELINE="true", EXPECTED_SHA=sha, GITHUB_SHA=sha,
                   ACTUAL_SHA=sha, EVENT_NAME="workflow_dispatch", EVENT_REF="refs/heads/effort/baseline",
                   RETAINED_DIGEST="", RETAINED_ATTACHMENT_ID="", RETAINED_ATTACHMENT_UUID="",
                   PROMOTION_PR="", PROMOTION_HEAD_SHA="", PROMOTION_BASE_SHA="")
        env.update(overrides)
        body = script(step(jobs()["scope"], "Bind diagnostic execution to a frozen non-main commit"))
        return self.run_shell(body, env, {"git": 'printf "%s\\n" "$ACTUAL_SHA"'})[0]

    def test_supplemental_workflow_gates_use_own_explicit_inputs(self):
        caller = jobs()['baseline_supplements']
        self.assertEqual(condition(caller), 'always()')
        self.assertIn('diagnostic_baseline: ${{ inputs.diagnostic_baseline }}', caller)
        self.assertIn('scope_result: ${{ needs.scope.result }}', caller)
        supplement = (ROOT / '.github/workflows/baseline-supplements.yml').read_text()
        self.assertIn('workflow_dispatch:', supplement)
        self.assertIn('sys.version_info[:3] == (3, 11, 2)', supplement)
        self.assertEqual(supplement.count("if: inputs.diagnostic_baseline == 'true' && inputs.scope_result == 'success'"), 2)
        self.assertNotIn('needs.scope', supplement)
        self.assertEqual(supplement.count('ref: ${{ inputs.expected_sha }}'), 2)
        self.assertEqual(supplement.count('workflow-transport.json'), 2)

    def test_deferred_failures_use_literal_boolean_and_strict_ordinary_verdict(self):
        text = WORKFLOW.read_text()
        self.assertNotRegex(text, r"continue-on-error:.*\$\{\{")
        for job_name in ('preflight', 'apple'):
            block = jobs()[job_name]
            deferred = re.findall(r"id: (baseline_\w+)\n        continue-on-error: true", block)
            self.assertTrue(deferred)
            for identity in deferred:
                verdict = step(block, 'Require ordinary outcome for ' + identity)
                self.assertIn("if: inputs.diagnostic_baseline != 'true' && steps." + identity + ".outcome != 'success'", verdict)
                self.assertIn('run: exit 1', verdict)

    def test_diagnostic_binding_requires_frozen_off_main_manual_commit(self):
        self.assertEqual(self.binding().returncode, 0)
        for bad in (dict(EVENT_REF="refs/heads/main"), dict(EVENT_REF="refs/tags/v1"),
                    dict(EVENT_NAME="pull_request"), dict(EXPECTED_SHA=""),
                    dict(EXPECTED_SHA="b" * 40), dict(ACTUAL_SHA="c" * 40),
                    dict(DIAGNOSTIC_BASELINE="yes")):
            with self.subTest(bad=bad):
                self.assertNotEqual(self.binding(**bad).returncode, 0)
        self.assertEqual(self.binding(DIAGNOSTIC_BASELINE="", EXPECTED_SHA="").returncode, 0)

    def test_diagnostic_binding_refuses_every_retained_or_promotion_input(self):
        for field in ("RETAINED_DIGEST", "RETAINED_ATTACHMENT_ID", "RETAINED_ATTACHMENT_UUID",
                      "PROMOTION_PR", "PROMOTION_HEAD_SHA", "PROMOTION_BASE_SHA"):
            with self.subTest(field=field):
                self.assertNotEqual(self.binding(**{field: "untrusted-proof"}).returncode, 0)

    def test_independent_diagnostic_lanes_survive_preflight_failure_only_after_bound_scope(self):
        independent = ("rust_compile", "check", "cluster_store_legacy", "cluster_topology",
                       "cluster_transport_recovery_contracts", "cluster_transport_recovery_voter",
                       "cluster_transport_recovery_learner", "cluster_wal", "cluster_daemon",
                       "web_layout", "vod_web", "android_jvm", "apple", "android_device", "package_smoke")
        for name in independent:
            with self.subTest(job=name):
                self.assertTrue(selected(jobs()[name]))
                self.assertFalse(selected(jobs()[name], **{"inputs.diagnostic_baseline": ""}))
                self.assertFalse(selected(jobs()[name], **{"needs.scope.result": "failure"}))
                self.assertTrue(selected(jobs()[name], **{"inputs.diagnostic_baseline": "", "needs.preflight.result": "success"}))
        self.assertTrue(selected(jobs()["vod_web"], **{"needs.web_layout.result": "failure"}))
        self.assertFalse(selected(jobs()["vod_web"], **{"inputs.diagnostic_baseline": "", "needs.preflight.result": "success", "needs.web_layout.result": "failure"}))
        for name in ("cluster_transport_recovery_voter", "cluster_transport_recovery_learner"):
            self.assertFalse(selected(jobs()[name], **{"needs.cluster_transport_recovery_contracts.result": "failure"}))

    def test_no_diagnostic_or_invalid_mode_can_publish(self):
        for mode in ("true", "invalid"):
            for name in ("publish", "publish_main", "badge"):
                for ref in ("refs/heads/main", "refs/tags/v1"):
                    with self.subTest(mode=mode, job=name, ref=ref):
                        self.assertFalse(selected(jobs()[name], **{"inputs.diagnostic_baseline": mode, "github.ref": ref}))
        self.assertTrue(selected(jobs()["publish"], **{"inputs.diagnostic_baseline": "", "github.ref": "refs/tags/v1"}))
        self.assertTrue(selected(jobs()["publish_main"], **{"inputs.diagnostic_baseline": ""}))
        self.assertTrue(selected(jobs()["badge"], **{"inputs.diagnostic_baseline": ""}))

    def test_diagnostic_mode_uses_legacy_without_resolving_repository_rollout(self):
        body = script(step(jobs()["scope"], "Resolve the fail-safe execution rollout mode"))
        result, output, _ = self.run_shell(body, {"DIAGNOSTIC_BASELINE": "true", "REQUESTED_MODE": "shadow"})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(output, "mode=legacy\n")

    def test_failed_validation_lint_does_not_hide_units_and_still_fails(self):
        body = script(step(jobs()["preflight"], "Check validation catalog and contract unit tests"))
        result, _, trace = self.run_shell(body, executables={
            "make": 'printf "lint\\n" >> "$TRACE"; exit 7',
            "python3": 'printf "units\\n" >> "$TRACE"; exit 0'})
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(trace.splitlines(), ["lint", "units"])

    def test_failed_node_script_does_not_hide_independent_script(self):
        body = script(step(jobs()["preflight"], "Check the shared player input contract"))
        result, _, trace = self.run_shell(body, executables={
            "node": 'printf "%s\\n" "$1" >> "$TRACE"; case "$1" in *player-input*) exit 3;; esac; exit 0'})
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(trace.splitlines(), ["tests/playback/player-input-contract.test.js", "tests/web/player-dom.test.js"])

    def test_final_preflight_verdict_rejects_hidden_or_missing_failures(self):
        body = script(step(jobs()["preflight"], "Require original diagnostic preflight outcomes"))
        for results, success in (("success " * 6, True), ("success " * 5 + "failure", False),
                                 ("success " * 5, False), ("success " * 5 + "skipped", False),
                                 ("", False)):
            with self.subTest(results=results):
                result, _, _ = self.run_shell(body, {"RESULTS": results})
                self.assertEqual(result.returncode == 0, success)

    def test_android_diagnostic_build_failure_cannot_be_hidden_or_push(self):
        for name in ("android_jvm", "android_device"):
            block = step(jobs()[name], "Reuse the pinned Android toolchain image from Forgejo")
            body = re.sub(r"\$\{\{.*?\}\}", "registry.invalid", script(block))
            for build_exit in (0, 9):
                with self.subTest(job=name, build_exit=build_exit):
                    result, _, trace = self.run_shell(body, {"REGISTRY_TOKEN": "", "GITHUB_ENV": "/dev/null"}, {
                        "sha256sum": 'printf "0123456789abcdef  Dockerfile\\n"',
                        "make": f'printf "build\\n" >> "$TRACE"; exit {build_exit}',
                        "docker": 'printf "docker %s\\n" "$1" >> "$TRACE"; if [ "$1" = pull ]; then exit 1; fi; exit 0'})
                    self.assertEqual(result.returncode == 0, build_exit == 0, result.stderr)
                    self.assertIn("build", trace)
                    self.assertNotIn("docker push", trace)


    def test_apple_exact_recipes_keep_destinations_independent_after_phone_failure(self):
        dry = subprocess.check_output(['make', '--no-print-directory', '-n', 'apple-test'], cwd=ROOT, text=True)
        block = step(jobs()['apple'], 'Run the shared XCTest suite on iPhone, iPad, and Apple TV')
        generator = script(block).split("<<'PYAPPLE'\n", 1)[1].split('\nPYAPPLE', 1)[0]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            (root / 'clients/apple').mkdir(parents=True)
            tools = root / 'bin'
            tools.mkdir()
            (tools / 'xcodegen').write_text('#!/bin/sh\nprintf "generate\\n" >> "$TRACE"\n')
            (tools / 'xcodebuild').write_text(
                '#!/bin/sh\nprintf "%s|%s\\n" "$PWD" "$*" >> "$TRACE"\n'
                'case "$*" in *id=phone*test-without-building) exit 7;; esac\nexit 0\n')
            for tool in tools.iterdir():
                tool.chmod(0o755)
            output = root / 'evidence'
            output.mkdir()
            context = dict(candidate_sha='a' * 40, candidate_tree='b' * 40)
            environment = dict(PATH=str(tools) + os.pathsep + os.environ['PATH'], TRACE=str(root / 'trace'),
                               PLURX_BASELINE_DIR=str(output), PLURX_TEST_CANDIDATE_SHA=context['candidate_sha'],
                               PLURX_TEST_CANDIDATE_TREE=context['candidate_tree'],
                               APPLE_IOS_SIM='platform=iOS Simulator,id=phone',
                               APPLE_IPAD_SIM='platform=iOS Simulator,id=pad',
                               APPLE_TVOS_SIM='platform=tvOS Simulator,id=tv')
            reports = []
            def invoke(argv):
                report = baseline_commands.run_manifest(argv[argv.index('--manifest') + 1],
                                                       argv[argv.index('--output') + 1], root=root)
                reports.append(report)
                return 0 if report['success'] else 1
            with mock.patch.dict(os.environ, environment), \
                 mock.patch.object(subprocess, 'check_output', return_value=dry), \
                 mock.patch.object(subprocess, 'call', side_effect=invoke), \
                 mock.patch.object(baseline_commands, 'source_identity', return_value=context):
                with self.assertRaises(SystemExit) as stopped:
                    exec(compile(generator, '<apple-workflow-generator>', 'exec'), {})
                self.assertEqual(stopped.exception.code, 1)
            self.assertEqual([cell['id'] for cell in reports[1]['cells']], ['iphone', 'ipad', 'tvos'])
            self.assertEqual([cell['returncode'] for cell in reports[1]['cells']], [7, 0, 0])
            trace = (root / 'trace').read_text()
            self.assertEqual(trace.count('generate'), 1)
            for destination in ('phone', 'pad', 'tv'):
                self.assertIn('id=' + destination, trace)
            self.assertNotIn('clients/apple/clients/apple', trace)


if __name__ == "__main__":
    unittest.main()
