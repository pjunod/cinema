"""Fast-lane throughput contracts (docs/ci/CI-THROUGHPUT.md, RUST-GATE-IMAGE.md).

Each case pins one change that stopped costing lane runs: a push cancels the
obsolete run, derivable paperwork is fixed instead of failed, the PR
description is read from the event file, and the Rust gate runs once, in its
pinned image, sized from its own container.
"""

from __future__ import annotations

import importlib.machinery
import importlib.util
import json
import os
import re
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from test_contracts import workflow_job_blocks, workflow_step_blocks, workflow_step_scalar

ROOT = Path(__file__).resolve().parents[2]
LANE = ROOT / ".github" / "workflows" / "main-fast-lane.yml"
EFFORT = ROOT / ".github" / "workflows" / "effort-ci.yml"
PAPERWORK_PR = ROOT / ".github" / "workflows" / "paperwork.yml"
# Fixture documents, spelled so the repository-wide docs/ reference sweep
# (test_docs_index) does not read them as links to real documents.
FIXTURE_DOCS = "docs" + "/ci"


def load_script(name: str):
    path = ROOT / "scripts" / name
    loader = importlib.machinery.SourceFileLoader(name.replace("-", "_"), str(path))
    spec = importlib.util.spec_from_loader(loader.name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


def job(path: Path, name: str) -> str:
    return workflow_job_blocks(str(path.relative_to(ROOT)))[name]


def header(path: Path, name: str) -> str:
    return job(path, name).split("    steps:\n", 1)[0]


def steps(path: Path, name: str) -> dict[str, str]:
    return workflow_step_blocks(job(path, name))


def run_text(step: str) -> str:
    """A step's `run:`, scalar or literal block, as one string."""
    if "        run: |\n" in step:
        return "\n".join(line[10:] for line in step.split("        run: |\n", 1)[1].splitlines())
    return workflow_step_scalar(step, "run")


class SupersededRunTests(unittest.TestCase):
    def test_a_push_cancels_the_prs_in_flight_fast_lane_run(self):
        workflow = LANE.read_text(encoding="utf-8")
        self.assertEqual(
            re.findall(r"(?m)^concurrency:\n((?:  .*\n)+)", workflow),
            ["  group: main-fast-lane-${{ github.event.pull_request.number }}\n  cancel-in-progress: true\n"],
        )


class RustGateTests(unittest.TestCase):
    def test_every_rust_gate_lane_pins_one_published_image(self):
        image = load_script("rust-gate-image")
        self.assertEqual(image.check(), [])
        for path in (LANE, EFFORT):
            with self.subTest(workflow=path.name):
                head = header(path, "rust_compile")
                image_line = re.findall(r"(?m)^      image: (.+)$", head)
                self.assertEqual(len(image_line), 1)
                self.assertRegex(image_line[0], r"/noirr/plurx-rust-gate@sha256:[0-9a-f]{64}$")
                self.assertNotRegex(image_line[0], r"@sha256:0{64}$")
                self.assertIn("        password: ${{ secrets.CI_REGISTRY_PULL_TOKEN }}\n", head)
                names = list(steps(path, "rust_compile"))
                self.assertNotIn("Install Rust gate prerequisites", names)
                check = run_text(steps(path, "rust_compile")["Require the gate image built from this tree"])
                self.assertIn("sha256sum Dockerfile.rust-gate", check)
                self.assertIn("/etc/plurx/rust-gate-input", check)

    def test_pin_rewrites_every_lane_and_check_refuses_a_split(self):
        image = load_script("rust-gate-image")
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for path in image.PINNED:
                (root / path).parent.mkdir(parents=True, exist_ok=True)
                (root / path).write_text("image: r/noirr/plurx-rust-gate@sha256:" + "a" * 64 + "\n")
            with mock.patch.object(image, "ROOT", root):
                self.assertEqual(image.check(), [])
                (root / image.PINNED[0]).write_text("image: r/noirr/plurx-rust-gate@sha256:" + "b" * 64 + "\n")
                self.assertTrue(image.check())
                self.assertEqual(image.pin("sha256:" + "c" * 64), [image.PINNED[0], image.PINNED[1]])
                self.assertEqual(image.check(), [])
                with self.assertRaises(SystemExit):
                    image.pin("latest")

    def test_cargo_jobs_follow_the_container_limits(self):
        jobs = load_script("ci-cargo-jobs")
        gib = 1024**3
        cases = [
            ("400000 100000", str(20 * gib), 4),   # the gate's container: was CARGO_BUILD_JOBS=1
            ("400000 100000", str(8 * gib), 1),    # memory binds first
            ("1600000 100000", str(40 * gib), 8),
            ("150000 100000", str(20 * gib), 1),   # a fractional CPU rounds down, never to 0
        ]
        for cpu, memory, expected in cases:
            with self.subTest(cpu=cpu, memory=memory), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                (root / "cpu.max").write_text(cpu + "\n")
                (root / "memory.max").write_text(memory + "\n")
                self.assertEqual(jobs.jobs(root)[0], expected)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "cpu.max").write_text("max 100000\n")
            (root / "memory.max").write_text("max\n")
            self.assertEqual(jobs.jobs(root)[0], len(os.sched_getaffinity(0)))
        for path in (LANE, EFFORT):
            with self.subTest(workflow=path.name):
                self.assertNotIn("CARGO_BUILD_JOBS", header(path, "rust_compile").split("    runs-on:", 1)[1])
                sized = steps(path, "rust_compile")["Size Cargo parallelism from the container's limits"]
                self.assertEqual(run_text(sized), 'python3 scripts/ci-cargo-jobs >> "$GITHUB_ENV"')

    def test_the_fast_gate_type_checks_the_workspace_once(self):
        makefile = (ROOT / "Makefile").read_text(encoding="utf-8")
        target = makefile.split("\nfast-rust-gate:", 1)[1].split("\n\n", 1)[0]
        self.assertIn("clippy --workspace --locked --all-targets -- -D warnings", target)
        self.assertNotIn("cargo check", target)
        self.assertNotIn("effort-rust-check", target)
        gate = steps(LANE, "rust_compile")
        self.assertEqual(run_text(gate["Run the Rust gate"]), "make fast-rust-gate")
        everything = job(LANE, "rust_compile")
        self.assertNotIn("effort-rust-check", everything)
        self.assertNotIn("make lint", everything)


class PullRequestEventTests(unittest.TestCase):
    def test_the_description_comes_from_the_event_file(self):
        sys.path.insert(0, str(ROOT))
        from validation import pr_event

        body = "Regression-Test: tests/x.py::y\n" + "x" * (200 * 1024)
        with tempfile.TemporaryDirectory() as temporary:
            event = Path(temporary) / "event.json"
            event.write_text(json.dumps({"pull_request": {"body": body, "title": "fix(x): y"}}))
            environment = {k: v for k, v in os.environ.items() if k not in ("PR_BODY", "PR_TITLE")}
            environment["GITHUB_EVENT_PATH"] = str(event)
            with mock.patch.dict(os.environ, environment, clear=True):
                self.assertEqual(pr_event.body(), body)
                self.assertEqual(pr_event.title(), "fix(x): y")
            with mock.patch.dict(os.environ, {**environment, "PR_BODY": "explicit"}, clear=True):
                self.assertEqual(pr_event.body(), "explicit")

    def test_no_workflow_passes_the_description_through_the_environment(self):
        for path in sorted((ROOT / ".github" / "workflows").glob("*.yml")):
            with self.subTest(workflow=path.name):
                self.assertNotIn("github.event.pull_request.body", path.read_text(encoding="utf-8"))


class PaperworkFixTests(unittest.TestCase):
    def setUp(self):
        self.fix = load_script("paperwork-fix")
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.patch = mock.patch.object(self.fix, "ROOT", self.root)
        self.patch.start()

    def tearDown(self):
        self.patch.stop()
        self.temporary.cleanup()

    def git(self, *args):
        return subprocess.run(["git", "-c", "user.name=t", "-c", "user.email=t@t", *args], cwd=self.root,
                               check=True, capture_output=True, text=True).stdout

    def write(self, path, text):
        (self.root / path).parent.mkdir(parents=True, exist_ok=True)
        (self.root / path).write_text(text, encoding="utf-8")

    def repository(self):
        self.git("init", "-q", "-b", "main")
        self.write("clients/android/app/build.gradle.kts", 'android {\n        versionCode = 199\n        versionName = "0.8.0"\n}\n')
        self.write("clients/android/README.md", "# Android\n\n> Status: **v0.8.0**, build `199` — a change.\n")
        self.write(f"{FIXTURE_DOCS}/README.md", "# ci\n\n| File | Answers | |\n|---|---|---|\n| [A.md](A.md) | a | live |\n")
        self.write(f"{FIXTURE_DOCS}/A.md", "# A\n")
        self.write("src/code.rs", "fn main() {}\n")
        self.git("add", "-A")
        self.git("commit", "-q", "-m", "base")

    def test_the_android_counter_is_raised_above_the_merge_target(self):
        self.repository()
        self.git("branch", "target")
        self.git("checkout", "-q", "target")
        self.write("clients/android/app/build.gradle.kts", 'android {\n        versionCode = 200\n        versionName = "0.8.0"\n}\n')
        self.git("commit", "-qam", "main took 200")
        self.git("checkout", "-q", "main")
        self.assertEqual(self.fix.fix_android("target"),
                         ["clients/android/app/build.gradle.kts", "clients/android/README.md"])
        self.assertIn("versionCode = 201", (self.root / "clients/android/app/build.gradle.kts").read_text())
        self.assertIn("build `201` — a change.", (self.root / "clients/android/README.md").read_text())
        self.assertEqual(self.fix.non_bookkeeping(self.fix.dirty_paths()), [])
        self.assertEqual(self.fix.fix_android("target"), [], "a claim above the target is kept")

    def test_only_numbers_or_markdown_may_change(self):
        self.repository()
        self.write("src/code.rs", "fn main() { println!(\"x\"); }\n")
        self.write("clients/android/app/build.gradle.kts", 'android {\n        versionCode = 300\n        versionName = "0.9.0"\n}\n')
        self.assertTrue(self.fix.numbers_only("versionCode = 1\n", "versionCode = 2\n"))
        self.assertFalse(self.fix.numbers_only("versionCode = 1\n", "versionCode = 2\nextra\n"))
        self.assertFalse(self.fix.numbers_only('name = "a1"\n', 'name = "b1"\n'))
        refused = self.fix.non_bookkeeping(["src/code.rs", "clients/android/app/build.gradle.kts"])
        self.assertIn("src/code.rs", refused)
        self.write("new.txt", "x")
        self.assertTrue(any("added" in item for item in self.fix.non_bookkeeping([])))

    def test_a_missing_index_row_is_derived_from_the_document(self):
        self.repository()
        self.write(f"{FIXTURE_DOCS}/NEW.md", "# A new thing\n\n**Status:** built · today\n")
        row = self.fix.index_row(f"{FIXTURE_DOCS}/NEW.md", 3)
        self.assertEqual(row, "| [NEW.md](NEW.md) | A new thing | built |")
        self.assertEqual(self.fix.index_row(f"{FIXTURE_DOCS}/A.md", 2), "| [A.md](A.md) | A |")

    def test_a_fix_commit_is_never_fixed_again(self):
        self.repository()
        self.assertFalse(self.fix.head_is_fix())
        self.git("commit", "-q", "--allow-empty", "-m", "chore(paperwork): x\n\nPaperwork-Fix: run 1")
        self.assertTrue(self.fix.head_is_fix())
        head = self.git("rev-parse", "HEAD").strip()
        args = mock.Mock(head_sha=head, base=head, merge_target="main", run_url="u")
        self.assertEqual(self.fix.ci(args), 1)

    def test_pr_workflows_never_receive_a_persistent_publisher_token(self):
        for path in (LANE, PAPERWORK_PR):
            self.assertNotIn("secrets.PAPERWORK_PUSH_TOKEN", path.read_text())

    def test_both_paperwork_jobs_fix_then_push_and_never_loop(self):
        for path in (LANE, PAPERWORK_PR):
            with self.subTest(workflow=path.name):
                run = run_text(steps(path, "paperwork")["Check index rows, release counters, names and Regression-Test fields"])
                self.assertLess(run.index("scripts/paperwork --base"), run.index("scripts/paperwork-fix --ci"))
                self.assertIn('--head-sha "$PR_HEAD_SHA"', run)
                self.assertIn('[ "$SAME_REPO" = true ] || exit 1', run)
                # The push is the script's leased, tested one; the workflow
                # neither pushes nor puts a credential on a command line.
                self.assertIn('python3 scripts/paperwork-fix --push --head-ref "$HEAD_REF" --base-ref "$BASE_REF" \\\n'
                              '  --head-sha "$PR_HEAD_SHA" || exit 1', run)
                self.assertNotRegex(run, r"\bgit\b[^\n]*\bpush\b")
                self.assertNotIn("extraheader", run.split("scripts/paperwork-fix --ci", 1)[1])
                self.assertLess(run.index("scripts/paperwork-fix --ci"), run.index("scripts/paperwork-fix --push"))
        effort_fix = [line for line in run_text(steps(PAPERWORK_PR, "paperwork")[
            "Check index rows, release counters, names and Regression-Test fields"]).splitlines()
            if "scripts/paperwork-fix --ci" in line]
        self.assertEqual(len(effort_fix), 1)
        self.assertIn("--counter-target origin/main", effort_fix[0])
        with mock.patch("sys.stdout"), self.assertRaises(SystemExit) as done:
            self.fix.main(["--counter-target", "origin/main", "--help"])
        self.assertEqual(done.exception.code, 0)


class PaperworkFixCommitAndPushTests(unittest.TestCase):
    """ci() and push() end to end, against a local bare remote."""

    # The fixture, not the tests, of PaperworkFixTests.
    tearDown = PaperworkFixTests.tearDown
    git = PaperworkFixTests.git
    write = PaperworkFixTests.write
    repository = PaperworkFixTests.repository

    def setUp(self):
        PaperworkFixTests.setUp(self)
        self.repository()
        self.remote = Path(self.temporary.name + "-remote.git")
        subprocess.run(["git", "init", "-q", "--bare", str(self.remote)], check=True)
        self.addCleanup(subprocess.run, ["rm", "-rf", str(self.remote)])
        self.git("remote", "add", "origin", str(self.remote))
        self.git("checkout", "-q", "-b", "work")
        self.write("src/work.rs", "fn work() {}\n")
        self.git("add", "-A")
        self.git("commit", "-q", "-m", "feat: work")
        self.git("push", "-q", "origin", "main", "work")
        self.head = self.git("rev-parse", "HEAD").strip()

    def args(self, **extra):
        values = dict(head_sha=self.head, base=self.head, merge_target="main", counter_target=None,
                      run_url="u", head_ref="work", base_ref="main")
        values.update(extra)
        return mock.Mock(**values)

    def fixed(self, changes):
        def apply(base, target):
            for path, text in changes.items():
                self.write(path, text)
            return sorted(changes)
        return mock.patch.object(self.fix, "apply", side_effect=apply)

    def remote_head(self, ref="work"):
        return subprocess.run(["git", "--git-dir", str(self.remote), "rev-parse", ref],
                              capture_output=True, text=True).stdout.strip()

    def commit_fix(self):
        with self.fixed({f"{FIXTURE_DOCS}/README.md": "# ci, fixed\n"}), mock.patch("sys.stdout"):
            self.assertEqual(self.fix.ci(self.args()), 0)

    def test_a_bookkeeping_fix_is_committed_with_its_trailer(self):
        self.commit_fix()
        self.assertTrue(self.fix.head_is_fix())
        self.assertEqual(self.git("rev-parse", "HEAD^").strip(), self.head)
        self.assertEqual(self.git("show", "--name-only", "--format=", "HEAD").split(), [f"{FIXTURE_DOCS}/README.md"])

    def test_a_code_change_is_never_committed(self):
        with self.fixed({"src/code.rs": "fn main() { evil(); }\n"}), mock.patch("sys.stdout"):
            self.assertEqual(self.fix.ci(self.args()), 1)
        self.assertEqual(self.git("rev-parse", "HEAD").strip(), self.head)

    def test_a_checkout_that_is_not_the_head_is_never_fixed(self):
        with self.fixed({f"{FIXTURE_DOCS}/README.md": "# x\n"}) as apply, mock.patch("sys.stdout"):
            self.assertEqual(self.fix.ci(self.args(head_sha="0" * 40)), 1)
        apply.assert_not_called()
        self.assertEqual(self.git("rev-parse", "HEAD").strip(), self.head)

    def push(self, **extra):
        with mock.patch.dict(os.environ, {"GITHUB_TOKEN": "job-token"}), mock.patch("sys.stdout"):
            return self.fix.push(self.args(**extra))

    def test_the_fix_is_pushed_to_the_head_branch(self):
        self.commit_fix()
        self.assertEqual(self.push(), 0)
        self.assertEqual(self.remote_head(), self.git("rev-parse", "HEAD").strip())

    def test_a_force_push_that_dropped_the_head_is_not_undone(self):
        self.commit_fix()
        # The author dropped the head commit while the run was fixing it.
        subprocess.run(["git", "--git-dir", str(self.remote), "update-ref", "refs/heads/work", self.head + "^"],
                       check=True)
        self.assertEqual(self.push(), 1)
        self.assertEqual(self.remote_head(), self.git("rev-parse", self.head + "^").strip())

    def test_only_a_fix_on_the_head_goes_only_to_a_work_branch(self):
        self.assertEqual(self.push(), 1, "HEAD is not a fix commit")
        self.commit_fix()
        for ref in ("main", "refs/pull/7/head", "", "bad..name"):
            with self.subTest(ref=ref):
                # Refused by name, not only by the lease finding no such branch.
                self.assertIsNotNone(self.fix.push_refusal(ref, self.head, "main"))
                self.assertEqual(self.push(head_ref=ref), 1)
        self.assertIsNone(self.fix.push_refusal("work", self.head, "main"))
        self.assertEqual(self.push(base_ref="work"), 1, "the base branch is never pushed")
        self.assertEqual(self.push(head_sha="0" * 40), 1, "the fix must sit on the checked head")
        self.assertEqual(self.remote_head(), self.head)
        self.assertEqual(self.remote_head("main"), self.git("rev-parse", "main").strip())

    def test_the_token_never_reaches_the_command_line(self):
        command, environment = self.fix.push_command("work", self.head, "s3cret")
        self.assertIn(f"--force-with-lease=refs/heads/work:{self.head}", command)
        self.assertNotIn("--force", [part for part in command if not part.startswith("--force-with-lease=")])
        encoded = self.fix.basic_auth("s3cret")
        self.assertFalse(any("s3cret" in part or encoded in part for part in command))
        values = [environment[f"GIT_CONFIG_VALUE_{i}"] for i in range(int(environment["GIT_CONFIG_COUNT"]))]
        self.assertIn(f"AUTHORIZATION: basic {encoded}", values)
        self.assertIn(os.devnull, values, "repository hooks must not run with the token")


if __name__ == "__main__":
    unittest.main()
