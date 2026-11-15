"""`scripts/land` is the one way a pull request reaches main with the train off.

A corrective landing carries its pull request's `Regression-Test:` lines in
the landing commit's message, because that is where `make history-check`
reads them once the commit is on main. The raw merge API lands the title
alone, and thirteen such landings in two days (#1596, #1599, then eleven on
2026-11-15) each cost a permanent errata row. These tests pin what the script
refuses before merging, the exact message it merges with, what it checks
afterwards, and the push-to-main job that catches a landing made some other
way.
"""

from __future__ import annotations

import contextlib
import io
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock

from validation import landing

ROOT = Path(__file__).resolve().parents[2]
FIXTURES = Path(__file__).resolve().parent / "land-fixtures"
LANDED = "f" * 40
RAFT_CLIENT = "vendor/hiqlite/src/network/raft_client.rs"
RAFT_TESTS = (
    "read_confirm_cycle_reuses_one_socket_across_answered_rpcs",
    "dropped_healthy_client_is_leased_without_a_new_socket",
    "timed_out_rpc_closes_its_connection_instead_of_parking_it",
    "parked_supervisor_closes_its_socket_at_the_idle_deadline",
    "client_reset_during_lease_is_closed_not_reused",
    "parked_connection_whose_socket_ends_exits_and_is_not_leased",
)


def later() -> landing.ForgeError:
    return landing.ForgeError(405, json.dumps({"message": "Please try again later"}))


class FakeForge:
    """A forge holding one pull request; `answers` are consumed one per merge attempt."""

    def __init__(self, pull: dict, answers: tuple = (), keep_message: bool = True):
        self.state = pull
        self.answers = list(answers)
        self.keep_message = keep_message
        self.merges: list[tuple] = []
        self.message = ""

    def pull(self, number: int) -> dict:
        assert number == self.state["number"]
        return dict(self.state)

    def merge(self, number: int, head: str, title: str, message: str) -> None:
        self.merges.append((number, head, title, message))
        answer = self.answers.pop(0) if self.answers else None
        if answer is not None:
            raise answer
        self.message = title + ("\n\n" + message if message and self.keep_message else "") + "\n"
        self.state.update(merged=True, state="closed", merge_commit_sha=LANDED)

    def commit_message(self, sha: str) -> str:
        assert sha == LANDED
        return self.message


class Repo:
    """A throwaway repository with a `main` and whatever branches a test adds."""

    def __init__(self, case: unittest.TestCase):
        directory = tempfile.TemporaryDirectory()
        case.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.name", "Land Test")
        self.git("config", "user.email", "land@example.invalid")
        self.git("config", "commit.gpgsign", "false")
        self.write("README.md", "base\n")
        self.main = self.commit("chore: base")

    def git(self, *args: str) -> str:
        return subprocess.run(("git", *args), cwd=self.root, check=True, text=True,
                              capture_output=True).stdout.strip()

    def write(self, path: str, text: str) -> None:
        target = self.root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text, encoding="utf-8")

    def commit(self, subject: str) -> str:
        self.git("add", "-A")
        self.git("commit", "-q", "-m", subject)
        return self.git("rev-parse", "HEAD")

    def branch(self, name: str, subject: str, files: dict[str, str]) -> str:
        self.git("checkout", "-q", "-b", name, self.main)
        for path, text in files.items():
            self.write(path, text)
        head = self.commit(subject)
        self.git("checkout", "-q", "main")
        return head

    def fetch(self, head: str):
        return lambda _number: (self.git("rev-parse", "main"), head)


def pull(number: int, title: str, head: str, body: str, branch: str = "claude/the-fix") -> dict:
    return {"number": number, "title": title, "state": "open", "merged": False, "body": body,
            "base": {"ref": "main"}, "head": {"ref": branch, "sha": head}}


class LandCase(unittest.TestCase):
    def setUp(self):
        self.repo = Repo(self)
        self.head = self.repo.branch(
            "claude/the-fix", "fix(thing): stop the thing",
            {"tests/test_thing.py": "def test_the_thing_stops():\n    assert True\n\n"
                                    "def test_the_thing_stays_stopped():\n    assert True\n"},
        )
        self.body = (
            "Why and how.\n\n"
            "Regression-Test: tests/test_thing.py::test_the_thing_stops\n"
            "  Regression-Test: tests/test_thing.py::test_the_thing_stays_stopped  \n\n"
            "Generated elsewhere.\n"
        )
        self.said: list[str] = []
        self.slept: list[float] = []

    def land(self, forge: FakeForge, head: str | None = None, **options):
        head = head or self.head
        return landing.land(forge, forge.state["number"], head, self.repo.root,
                            fetch=self.repo.fetch(forge.state["head"]["sha"]),
                            sleep=self.slept.append, say=self.said.append, **options)

    def refused(self, forge: FakeForge, head: str | None = None, kind=landing.LandRefused) -> str:
        with self.assertRaises(kind) as raised:
            self.land(forge, head)
        self.assertEqual(forge.merges, [], "a refused pull request must never reach the merge API")
        return str(raised.exception)

    # -- refusals ----------------------------------------------------------------

    def test_a_draft_title_is_refused(self):
        for title in ("WIP: fix(thing): stop the thing", "wip: fix(thing): stop", "[WIP] fix(thing): stop"):
            with self.subTest(title=title):
                why = self.refused(FakeForge(pull(7, title, self.head, self.body)))
                self.assertIn("still a draft", why)

    def test_a_head_other_than_the_reviewed_one_is_refused(self):
        moved = self.repo.branch("claude/pushed-since", "fix(thing): one more", {"x.txt": "x\n"})
        why = self.refused(FakeForge(pull(7, "fix(thing): stop the thing", moved, self.body)))
        self.assertIn(f"head is {moved}, not the --head {self.head}", why)

    def test_a_corrective_pull_request_with_no_line_is_refused(self):
        why = self.refused(FakeForge(pull(7, "fix(thing): stop the thing", self.head, "No field here.\n")))
        self.assertIn("carries no Regression-Test: <path>::<test> line", why)
        why = self.refused(FakeForge(pull(7, "perf(thing): stop sooner", self.head, "")))
        self.assertIn("perf(thing): stop sooner", why)

    def test_a_feature_title_over_a_corrective_commit_still_needs_a_line(self):
        why = self.refused(FakeForge(pull(7, "feat(thing): a new thing", self.head, "")))
        self.assertIn("fix(thing): stop the thing", why)

    def test_a_line_that_does_not_resolve_in_the_merge_tree_is_refused(self):
        for line, expected in (
            ("tests/test_thing.py::test_renamed_away", "does not define"),
            ("tests/test_gone.py::test_the_thing_stops", "does not carry"),
            ("tests/test_thing.py", "is not <path>::<name>"),
        ):
            with self.subTest(line=line):
                body = self.body + f"Regression-Test: {line}\n"
                why = self.refused(FakeForge(pull(7, "fix(thing): stop the thing", self.head, body)))
                self.assertIn("do not resolve in the tree the merge will produce", why)
                self.assertIn(expected, why)

    def test_a_pull_request_that_conflicts_with_main_stops_as_a_conflict(self):
        head = self.repo.branch("claude/conflict", "fix(thing): edit the readme", {"README.md": "theirs\n"})
        self.repo.write("README.md", "ours\n")
        self.repo.commit("chore: main moved")
        state = {**pull(7, "fix(thing): edit the readme", head, self.body), "mergeable": False}
        why = self.refused(FakeForge(state), head, kind=landing.LandConflict)
        self.assertIn("conflicts with main", why)

    def test_closed_merged_and_other_targets_are_refused(self):
        for change, expected in (
            ({"state": "closed"}, "is closed, not open"),
            ({"merged": True, "merge_commit_sha": LANDED}, f"already merged as {LANDED}"),
            ({"base": {"ref": "effort/x"}}, "only lands on main"),
        ):
            with self.subTest(change=change):
                state = {**pull(7, "fix(thing): stop the thing", self.head, self.body), **change}
                self.assertIn(expected, self.refused(FakeForge(state)))

    # -- the landing -------------------------------------------------------------

    def test_a_good_pull_request_lands_with_exactly_its_title_and_trailers(self):
        forge = FakeForge(pull(7, "fix(thing): stop the thing", self.head, self.body))
        self.assertEqual(self.land(forge), LANDED)
        self.assertEqual(forge.merges, [(
            7,
            self.head,
            "Merge pull request 'fix(thing): stop the thing' (#7) from claude/the-fix into main",
            "Regression-Test: tests/test_thing.py::test_the_thing_stops\n"
            "Regression-Test: tests/test_thing.py::test_the_thing_stays_stopped",
        )])
        self.assertEqual(self.said[-1], f"landed #7 as {LANDED} with 2 Regression-Test lines")
        self.assertEqual(self.slept, [])

    def test_the_trailers_are_what_landing_lines_prints(self):
        from validation.regression_field import main as regression_field

        body = ("Prose.\n\nRegression-Test: tests/operations/test_land.py"
                "::test_the_trailers_are_what_landing_lines_prints\n")
        out = io.StringIO()
        with mock.patch.dict("os.environ", {"PR_TITLE": "", "PR_BODY": "", "GITHUB_EVENT_PATH": ""}), \
                contextlib.redirect_stdout(out):
            code = regression_field(["--landing-lines", "--body", body])
        self.assertEqual(code, 0)
        _subject, message = landing.landing_message("fix(x): y", 7, "b", landing.parse_regression_fields(body))
        self.assertEqual(message + "\n", out.getvalue())

    def test_an_older_git_builds_the_same_merge_tree_or_none(self):
        self.repo.write("other.txt", "main moved\n")
        main = self.repo.commit("chore: main moved")
        tree = landing.classic_merge_tree(self.repo.root, main, self.head)
        self.assertEqual(self.repo.git("ls-tree", "-r", "--name-only", tree).splitlines(),
                         ["README.md", "other.txt", "tests/test_thing.py"])
        if landing._writes_merge_trees(self.repo.root):
            self.assertEqual(landing.merge_tree(self.repo.root, main, self.head), (tree, True))
        clash = self.repo.branch("claude/clash", "fix(thing): clash", {"README.md": "theirs\n"})
        self.repo.write("README.md", "ours\n")
        main = self.repo.commit("chore: main moved again")
        self.assertIsNone(landing.classic_merge_tree(self.repo.root, main, clash))
        self.assertIsNone(landing.merge_tree(self.repo.root, main, clash)[0])
        self.assertEqual(self.repo.git("status", "--porcelain"), "", "the checkout must not be touched")

    def test_an_older_git_that_cannot_build_the_merge_asks_the_forge_and_falls_back_to_the_head(self):
        with mock.patch.object(landing, "merge_tree", return_value=(None, False)):
            forge = FakeForge({**pull(7, "fix(thing): stop the thing", self.head, self.body), "mergeable": False})
            self.assertIn("the forge reports it not mergeable", self.refused(forge, kind=landing.LandConflict))
            forge = FakeForge({**pull(7, "fix(thing): stop the thing", self.head, self.body), "mergeable": True})
            self.assertEqual(self.land(forge), LANDED)
        self.assertIn("judging the Regression-Test lines against the pull request head", self.said[0])

    def test_a_pull_request_that_needs_no_line_lands_with_the_title_alone(self):
        head = self.repo.branch("claude/docs", "docs: say more", {"notes/more.md": "more\n"})
        forge = FakeForge(pull(8, "docs: say more", head, "Prose only.\n", "claude/docs"))
        self.assertEqual(self.land(forge, head), LANDED)
        self.assertEqual(forge.merges, [(8, head, "Merge pull request 'docs: say more' (#8) from claude/docs into main", "")])

    def test_a_dry_run_prints_the_message_and_merges_nothing(self):
        forge = FakeForge(pull(7, "fix(thing): stop the thing", self.head, self.body))
        self.assertIsNone(self.land(forge, dry_run=True))
        self.assertEqual(forge.merges, [])
        self.assertEqual(self.said[:3], [
            "Merge pull request 'fix(thing): stop the thing' (#7) from claude/the-fix into main",
            "Regression-Test: tests/test_thing.py::test_the_thing_stops",
            "Regression-Test: tests/test_thing.py::test_the_thing_stays_stopped",
        ])

    def test_try_again_later_is_waited_out_and_then_lands(self):
        forge = FakeForge(pull(7, "fix(thing): stop the thing", self.head, self.body),
                          answers=(later(), later(), None))
        self.assertEqual(self.land(forge, retries=5, wait=3.0), LANDED)
        self.assertEqual(len(forge.merges), 3)
        self.assertEqual(self.slept, [3.0, 3.0])

    def test_try_again_later_is_bounded_and_says_nothing_merged(self):
        forge = FakeForge(pull(7, "fix(thing): stop the thing", self.head, self.body),
                          answers=tuple(later() for _ in range(10)))
        with self.assertRaises(landing.LandRefused) as raised:
            self.land(forge, retries=3, wait=2.0)
        self.assertEqual(len(forge.merges), 4)
        self.assertEqual(self.slept, [2.0, 2.0, 2.0])
        self.assertIn("still computing whether #7 merges", str(raised.exception))
        self.assertIn("nothing was merged", str(raised.exception))

    def test_a_forge_refusal_is_reported_and_a_conflict_is_called_one(self):
        for status, text, kind in (
            (405, "Does not have enough approvals", landing.LandRefused),
            (409, "head out of date", landing.LandRefused),
            (409, "There are merge conflicts", landing.LandConflict),
        ):
            with self.subTest(status=status, text=text):
                forge = FakeForge(pull(7, "fix(thing): stop the thing", self.head, self.body),
                                  answers=(landing.ForgeError(status, json.dumps({"message": text})),))
                with self.assertRaises(landing.LandRefused) as raised:
                    self.land(forge)
                self.assertIs(type(raised.exception), kind)
                self.assertIn(text, str(raised.exception))
                self.assertEqual(self.slept, [])

    def test_a_landing_that_lost_its_trailers_is_an_error_naming_them(self):
        forge = FakeForge(pull(7, "fix(thing): stop the thing", self.head, self.body), keep_message=False)
        with self.assertRaises(landing.LandedWrong) as raised:
            self.land(forge)
        why = str(raised.exception)
        self.assertIn(f"#7 landed as {LANDED}", why)
        self.assertIn("it lacks `Regression-Test: tests/test_thing.py::test_the_thing_stops`", why)
        self.assertIn("[[errata]]", why)

    def test_the_real_body_of_1596_yields_its_six_raft_client_trailers(self):
        body = (FIXTURES / "pr-1596-body.md").read_text(encoding="utf-8")
        source = "".join(f"#[tokio::test]\nasync fn {name}() {{}}\n\n" for name in RAFT_TESTS)
        title = "fix(hiqlite): lease idle Raft client connections instead of dialing per client"
        head = self.repo.branch("fix/raft-client-reuse-idle-connections", title, {RAFT_CLIENT: source})
        forge = FakeForge(pull(1596, title, head, body, "fix/raft-client-reuse-idle-connections"))
        self.assertEqual(self.land(forge, head), LANDED)
        (_number, _head, subject, message), = forge.merges
        self.assertEqual(
            subject,
            f"Merge pull request '{title}' (#1596) from fix/raft-client-reuse-idle-connections into main",
        )
        self.assertEqual(message.splitlines(), [f"Regression-Test: {RAFT_CLIENT}::{name}" for name in RAFT_TESTS])

    # -- the wire ----------------------------------------------------------------

    def test_the_merge_request_is_pinned_to_the_head_and_carries_the_message(self):
        response = mock.MagicMock()
        response.__enter__.return_value = response
        response.read.return_value = b""
        opener = mock.Mock(return_value=response)
        forge = landing.Forge("https://forge.example/", "owner/repo", "test-token", opener)
        forge.merge(7, self.head, "the title", "Regression-Test: a::b")
        request = opener.call_args.args[0]
        self.assertEqual(request.full_url, "https://forge.example/api/v1/repos/owner/repo/pulls/7/merge")
        self.assertEqual(request.get_method(), "POST")
        self.assertEqual(request.get_header("Authorization"), "token test-token")
        self.assertEqual(json.loads(request.data), {
            "Do": "merge",
            "MergeTitleField": "the title",
            "MergeMessageField": "Regression-Test: a::b",
            "head_commit_id": self.head,
        })

    def test_the_command_line_wants_a_full_head_and_never_prints_the_token(self):
        err = io.StringIO()
        with mock.patch.dict("os.environ", {"FORGEJO_TOKEN": "sekrit-token"}), contextlib.redirect_stderr(err):
            code = landing.main_land(["7", "--head", self.head[:9], "--root", str(self.repo.root)])
        self.assertEqual(code, 2)
        self.assertIn("full 40-character", err.getvalue())
        self.assertNotIn("sekrit-token", err.getvalue())


class PushCheckCase(unittest.TestCase):
    """The job that runs on every push to main."""

    def setUp(self):
        self.repo = Repo(self)
        self.before = self.repo.main

    def merge(self, number: int, title: str, branch: str, files: dict[str, str], message: str = "") -> str:
        self.repo.branch(branch, title, files)
        subject = f"Merge pull request '{title}' (#{number}) from {branch} into main"
        self.repo.git("merge", "-q", "--no-ff", branch, "-m", subject + ("\n\n" + message if message else ""))
        return self.repo.git("rev-parse", "HEAD")

    def errors(self, bodies: dict[int, str] | None = None) -> list[str]:
        return landing.audit_push(self.repo.root, self.before, "HEAD",
                                  (lambda number: bodies.get(number)) if bodies is not None else (lambda _n: None))

    TEST = {"tests/test_thing.py": "def test_the_thing_stops():\n    assert True\n"}
    LINE = "Regression-Test: tests/test_thing.py::test_the_thing_stops"

    def test_a_landing_carrying_its_lines_passes(self):
        self.merge(7, "fix(thing): stop the thing", "b", self.TEST, self.LINE)
        self.assertEqual(self.errors({7: f"Words.\n\n{self.LINE}\n"}), [])

    def test_a_corrective_landing_with_the_title_alone_fails_naming_the_pull_request(self):
        sha = self.merge(7, "fix(thing): stop the thing", "b", self.TEST)
        for bodies in (None, {7: f"Words.\n\n{self.LINE}\n"}):
            with self.subTest(bodies=bodies):
                errors = self.errors(bodies)
                self.assertTrue(errors)
                self.assertTrue(all(error.startswith(f"#7 landed as {sha[:9]}") for error in errors), errors)
        self.assertIn(self.LINE, "\n".join(self.errors({7: f"{self.LINE}\n"})))

    def test_a_feature_landing_that_dropped_its_description_lines_fails(self):
        self.merge(9, "feat(thing): more thing", "b", self.TEST)
        self.assertEqual(self.errors(None), [])
        errors = self.errors({9: f"{self.LINE}\n"})
        self.assertEqual(len(errors), 1)
        self.assertIn("#9 landed", errors[0])
        self.assertIn("without 1 of the Regression-Test lines", errors[0])

    def test_a_feature_landing_bringing_a_corrective_commit_needs_a_line(self):
        self.repo.branch("b", "fix(thing): stop the thing", self.TEST)
        self.repo.git("merge", "-q", "--no-ff", "b", "-m", "Merge pull request 'feat(thing): more' (#9) from b into main")
        errors = self.errors(None)
        self.assertEqual(len(errors), 1)
        self.assertIn("brings in 1 corrective commit: fix(thing): stop the thing", errors[0])

    def test_a_carried_line_must_resolve_in_the_landing_tree(self):
        self.merge(7, "fix(thing): stop the thing", "b", self.TEST,
                   "Regression-Test: tests/test_thing.py::test_something_else")
        errors = self.errors(None)
        self.assertEqual(len(errors), 1)
        self.assertIn("does not define", errors[0])

    def test_every_landing_in_the_push_is_judged_and_other_commits_are_not(self):
        self.merge(7, "fix(thing): stop the thing", "b", self.TEST, self.LINE)
        self.repo.write("notes.md", "n\n")
        self.repo.commit("fix(notes): pushed directly, not a landing")
        self.merge(8, "perf(thing): faster", "c", {"c.txt": "c\n"})
        errors = self.errors(None)
        self.assertEqual(len(errors), 1)
        self.assertIn("#8 landed", errors[0])

    def test_a_landing_an_errata_row_records_is_not_reported_again(self):
        sha = self.merge(7, "fix(thing): stop the thing", "b", self.TEST)
        self.repo.write("validation/merge-errata.toml",
                        f'version = 1\nenforce_after = "{self.before}"\n\n[[errata]]\ncommit = "{sha}"\n'
                        f'reason = "recorded"\n')
        self.assertEqual(self.errors(None), [])

    def test_an_unknown_before_judges_the_pushed_commit_alone(self):
        self.merge(7, "fix(thing): stop the thing", "b", self.TEST)
        for before in ("", "0" * 40, "1" * 40):
            with self.subTest(before=before):
                errors = landing.audit_push(self.repo.root, before, "HEAD", lambda _n: None)
                self.assertEqual(len(errors), 1)

    def test_the_command_fails_loudly_and_passes_quietly(self):
        self.merge(7, "fix(thing): stop the thing", "b", self.TEST)
        out, err = io.StringIO(), io.StringIO()
        clean = {"GITHUB_API_URL": "", "GITHUB_REPOSITORY": "", "LANDING_BEFORE": "", "LANDING_SHA": ""}
        with mock.patch.dict("os.environ", clean), contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = landing.main_check(["--root", str(self.repo.root), "--before", self.before])
        self.assertEqual(code, 1)
        self.assertIn("::error::#7 landed as", out.getvalue())
        self.assertIn("LANDING WITHOUT ITS REGRESSION-TEST LINES", err.getvalue())
        self.assertIn("scripts/land", err.getvalue())
        out = io.StringIO()
        with mock.patch.dict("os.environ", clean), contextlib.redirect_stdout(out):
            code = landing.main_check(["--root", str(self.repo.root), "--before", "HEAD"])
        self.assertEqual(code, 0)
        self.assertIn("landing check ok", out.getvalue())


class WiringCase(unittest.TestCase):
    def test_the_push_to_main_workflow_runs_the_landing_check_over_the_whole_push(self):
        workflow = (ROOT / ".github/workflows/landing-check.yml").read_text(encoding="utf-8")
        self.assertRegex(workflow, r"(?m)^on:\n  push:\n    branches: \[main\]\n")
        self.assertIn("fetch-depth: 0", workflow)
        self.assertIn("LANDING_BEFORE: ${{ github.event.before }}", workflow)
        self.assertIn("LANDING_SHA: ${{ github.sha }}", workflow)
        self.assertIn("python3 -m validation.landing check", workflow)

    def test_scripts_land_is_the_documented_off_mode_path(self):
        script = ROOT / "scripts/land"
        self.assertTrue(script.stat().st_mode & 0o111, "scripts/land must be executable")
        self.assertIn("main_land", script.read_text(encoding="utf-8"))
        for doc in ("AGENTS.md", "docs/ci/MERGE-TRAIN.md"):
            text = (ROOT / doc).read_text(encoding="utf-8")
            self.assertTrue("scripts/land <pr> --head <sha>" in text, doc)
            self.assertTrue("never the raw merge API" in text, doc)


if __name__ == "__main__":
    unittest.main()
