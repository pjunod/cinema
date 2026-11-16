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
import http.server
import io
import json
from pathlib import Path
import subprocess
import tempfile
import threading
import unittest
from unittest import mock

from validation import landing

ROOT = Path(__file__).resolve().parents[2]
FIXTURES = Path(__file__).resolve().parent / "land-fixtures"
GATE = "main fast lane / Main promotion gate (pull_request)"
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


class FakeForge:
    """A forge holding one pull request over a real repository.

    `answers` are consumed one per merge attempt; a merge that is let through
    is a real `git merge --no-ff` on the repository's main.
    """

    def __init__(self, repo: Repo, pull: dict, answers: tuple = (), keep_message: bool = True,
                 gate: str | None = "success", on_pull=None, lose_file: str | None = None):
        self.repo = repo
        self.state = pull
        self.answers = list(answers)
        self.keep_message = keep_message
        self.gate = gate
        self.on_pull = on_pull
        self.lose_file = lose_file
        self.reads = 0
        self.asked: list[str] = []
        self.merges: list[tuple] = []

    def pull(self, number: int) -> dict:
        assert number == self.state["number"]
        self.reads += 1
        if self.on_pull:
            self.on_pull(self)
        return dict(self.state)

    def statuses(self, sha: str) -> list[dict]:
        self.asked.append(sha)
        paperwork = {"id": 1, "context": "main fast lane / paperwork (pull_request)", "status": "success"}
        if self.gate is None:
            return [paperwork]
        return [paperwork, {"id": 2, "context": GATE, "status": self.gate,
                            "target_url": "https://forge.example/owner/repo/actions/runs/9"}]

    def merge(self, number: int, head: str, title: str, message: str) -> None:
        self.merges.append((number, head, title, message))
        answer = self.answers.pop(0) if self.answers else None
        if answer is not None:
            raise answer
        text = title + ("\n\n" + message if message and self.keep_message else "")
        self.repo.git("merge", "-q", "--no-ff", head, "-m", text)
        if self.lose_file:
            self.repo.git("rm", "-q", self.lose_file)
            self.repo.git("commit", "-q", "--amend", "--no-edit")
        self.state.update(merged=True, state="closed", merge_commit_sha=self.repo.git("rev-parse", "HEAD"))

    def commit_message(self, sha: str) -> str:
        return self.repo.git("show", "-s", "--format=%B", sha)

    @property
    def landed(self) -> str:
        return self.state["merge_commit_sha"]


def pull(number: int, title: str, head: str, body: str, branch: str = "claude/the-fix") -> dict:
    return {"number": number, "title": title, "state": "open", "merged": False, "body": body,
            "html_url": f"https://forge.example/owner/repo/pulls/{number}",
            "base": {"ref": "main"}, "head": {"ref": branch, "sha": head}}


class LandCase(unittest.TestCase):
    TITLE = "fix(thing): stop the thing"

    def setUp(self):
        self.repo = Repo(self)
        self.head = self.repo.branch(
            "claude/the-fix", self.TITLE,
            {"tests/test_thing.py": "def test_the_thing_stops():\n    assert True\n\n"
                                    "def test_the_thing_stays_stopped():\n    assert True\n"},
        )
        self.body = (
            "## Why\n\nWhy and how.\n\n"
            "Regression-Test: tests/test_thing.py::test_the_thing_stops\n"
            "  Regression-Test: tests/test_thing.py::test_the_thing_stays_stopped  \n\n"
            "Generated elsewhere.\n"
        )
        self.lines = [
            "Regression-Test: tests/test_thing.py::test_the_thing_stops",
            "Regression-Test: tests/test_thing.py::test_the_thing_stays_stopped",
        ]
        self.said: list[str] = []
        self.slept: list[float] = []

    def forge(self, title: str | None = None, head: str | None = None, body: str | None = None,
              change: dict | None = None, **options) -> FakeForge:
        state = pull(7, self.TITLE if title is None else title, head or self.head,
                     self.body if body is None else body)
        return FakeForge(self.repo, {**state, **(change or {})}, **options)

    def land(self, forge: FakeForge, head: str | None = None, **options):
        return landing.land(forge, forge.state["number"], head or self.head, self.repo.root,
                            fetch=self.repo.fetch(forge.state["head"]["sha"]),
                            fetch_commit=lambda _sha: None,
                            sleep=self.slept.append, say=self.said.append, **options)

    def refused(self, forge: FakeForge, head: str | None = None, kind=landing.LandRefused, **options) -> str:
        with self.assertRaises(kind) as raised:
            self.land(forge, head, **options)
        self.assertIs(type(raised.exception), kind)
        self.assertEqual(forge.merges, [], "a refused pull request must never reach the merge API")
        return str(raised.exception)

    # -- refusals ----------------------------------------------------------------

    def test_a_draft_title_is_refused(self):
        for title in ("WIP: fix(thing): stop the thing", "wip: fix(thing): stop", "[WIP] fix(thing): stop"):
            with self.subTest(title=title):
                self.assertIn("still a draft", self.refused(self.forge(title)))

    def test_a_head_other_than_the_reviewed_one_is_refused(self):
        moved = self.repo.branch("claude/pushed-since", "fix(thing): one more", {"x.txt": "x\n"})
        why = self.refused(self.forge(head=moved))
        self.assertIn(f"head is {moved}, not the --head {self.head}", why)

    def test_a_corrective_pull_request_with_no_line_is_refused(self):
        why = self.refused(self.forge(body="No field here.\n"))
        self.assertIn("carries no Regression-Test: <path>::<test> line", why)
        self.assertIn("perf(thing): stop sooner", self.refused(self.forge("perf(thing): stop sooner", body="")))

    def test_a_feature_title_over_a_corrective_commit_still_needs_a_line(self):
        self.assertIn(self.TITLE, self.refused(self.forge("feat(thing): a new thing", body="")))

    def test_a_line_that_does_not_resolve_in_the_merge_tree_is_refused(self):
        for line, expected in (
            ("tests/test_thing.py::test_renamed_away", "does not define"),
            ("tests/test_gone.py::test_the_thing_stops", "does not carry"),
            ("tests/test_thing.py", "is not <path>::<name>"),
        ):
            with self.subTest(line=line):
                why = self.refused(self.forge(body=self.body + f"Regression-Test: {line}\n"))
                self.assertIn("do not resolve in the tree the merge will produce", why)
                self.assertIn(expected, why)

    def test_a_line_is_judged_in_the_merge_tree_not_the_branch_head(self):
        self.repo.write("tests/test_main_only.py", "def test_only_main_has_this():\n    assert True\n")
        self.repo.commit("chore: main gains a test the branch never saw")
        body = self.body + "Regression-Test: tests/test_main_only.py::test_only_main_has_this\n"
        forge = self.forge(body=body)
        self.assertEqual(self.land(forge), forge.landed)

    def test_a_pull_request_that_conflicts_with_main_stops_as_a_conflict(self):
        head = self.repo.branch("claude/conflict", "fix(thing): edit the readme", {"README.md": "theirs\n"})
        self.repo.write("README.md", "ours\n")
        self.repo.commit("chore: main moved")
        forge = self.forge("fix(thing): edit the readme", head, change={"mergeable": False})
        self.assertIn("conflicts with main", self.refused(forge, head, kind=landing.LandConflict))

    def test_a_merge_tree_that_cannot_be_built_is_refused_never_judged_at_the_head(self):
        with mock.patch.object(landing, "merge_tree", return_value=(None, False)):
            why = self.refused(self.forge(change={"mergeable": True}))
            self.assertIn("cannot build the tree merging #7 into main", why)
            self.assertIn("nothing was merged", why)
            self.assertIn("Git 2.38 or newer", why)
            self.refused(self.forge(change={"mergeable": False}), kind=landing.LandConflict)
        with mock.patch.object(landing, "merge_tree", return_value=(None, True)):
            self.refused(self.forge(change={"mergeable": True}), kind=landing.LandConflict)

    def test_an_older_git_builds_the_same_merge_tree_or_none(self):
        self.repo.write("other.txt", "main moved\n")
        main = self.repo.commit("chore: main moved")
        tree = landing.classic_merge_tree(self.repo.root, main, self.head)
        self.assertEqual(self.repo.git("ls-tree", "-r", "--name-only", tree).splitlines(),
                         ["README.md", "other.txt", "tests/test_thing.py"])
        if landing._git_version(self.repo.root) >= (2, 38):
            self.assertEqual(landing.merge_tree(self.repo.root, main, self.head), (tree, True))
        clash = self.repo.branch("claude/clash", "fix(thing): clash", {"README.md": "theirs\n"})
        self.repo.write("README.md", "ours\n")
        main = self.repo.commit("chore: main moved again")
        self.assertIsNone(landing.classic_merge_tree(self.repo.root, main, clash))
        self.assertIsNone(landing.merge_tree(self.repo.root, main, clash)[0])
        self.assertEqual(self.repo.git("status", "--porcelain"), "", "the checkout must not be touched")

    def test_more_than_one_merge_base_is_not_built_by_the_older_git_path(self):
        with mock.patch.object(landing, "_git", side_effect=lambda _root, *args: "a\nb\n"
                               if args[:2] == ("merge-base", "--all") else self.fail(args)):
            self.assertIsNone(landing.classic_merge_tree(self.repo.root, "x", "y"))

    def test_closed_merged_and_other_targets_are_refused(self):
        for change, expected in (
            ({"state": "closed"}, "is closed, not open"),
            ({"merged": True, "merge_commit_sha": "f" * 40}, f"already merged as {'f' * 40}"),
            ({"base": {"ref": "effort/x"}}, "only lands on main"),
        ):
            with self.subTest(change=change):
                self.assertIn(expected, self.refused(self.forge(change=change)))

    # -- the head has passed -----------------------------------------------------

    def test_a_head_whose_gate_is_not_green_is_refused(self):
        for gate, expected in (
            ("failure", "is failure"),
            ("error", "is error"),
            ("pending", "is still running"),
            ("skipped", "was skipped"),
            (None, "has reported no 'main fast lane / Main promotion gate' status"),
        ):
            with self.subTest(gate=gate):
                forge = self.forge(gate=gate)
                why = self.refused(forge)
                self.assertIn(f"#7 head {self.head} has not passed", why)
                self.assertIn(expected, why)
                self.assertIn("--accept-untested", why)
                self.assertEqual(forge.asked, [self.head])

    def test_the_gate_is_the_latest_status_of_its_own_context(self):
        old = {"id": 5, "context": GATE, "status": "failure", "updated_at": "2026-11-15T10:00:00Z"}
        new = {"id": 9, "context": GATE, "status": "success", "updated_at": "2026-11-15T11:00:00Z",
               "target_url": "u"}
        other = {"id": 99, "context": "main fast lane / paperwork (pull_request)", "status": "failure"}
        self.assertEqual(landing.gate_state([old, other, new]), ("success", "u"))
        self.assertEqual(landing.gate_state([new, old])[0], "success")
        self.assertEqual(landing.gate_state([other]), ("missing", ""))
        self.assertEqual(landing.gate_state([{"context": "main fast lane / Main promotion gate", "status": "pending"}])[0],
                         "pending")

    def test_an_untested_head_lands_only_with_a_reason_that_is_printed_and_recorded(self):
        forge = self.forge(gate="failure")
        sha = self.land(forge, untested="Paul ordered a direct merge of the hotfix")
        self.assertEqual(sha, forge.landed)
        self.assertTrue(self.said[0].startswith("WARNING: LANDING AN UNTESTED HEAD. #7"), self.said[0])
        self.assertIn("is failure", self.said[0])
        self.assertEqual(self.said[1], "WARNING: accepted with --accept-untested: Paul ordered a direct merge of the hotfix")
        self.assertEqual(
            forge.commit_message(sha).splitlines()[-2:],
            ["Reviewed-on: https://forge.example/owner/repo/pulls/7",
             "Landed-Untested: Paul ordered a direct merge of the hotfix"],
        )

    def test_a_green_head_records_no_untested_reason_even_when_one_is_offered(self):
        forge = self.forge()
        self.land(forge, untested="not needed")
        self.assertNotIn("Landed-Untested", forge.merges[0][3])
        self.assertFalse(any(line.startswith("WARNING") for line in self.said))

    # -- the landing -------------------------------------------------------------

    def test_a_good_pull_request_lands_with_exactly_its_title_and_trailers(self):
        forge = self.forge()
        sha = self.land(forge)
        self.assertEqual(sha, self.repo.git("rev-parse", "main"))
        self.assertEqual(forge.merges, [(
            7,
            self.head,
            "Merge pull request 'fix(thing): stop the thing' (#7) from claude/the-fix into main",
            self.body.strip() + "\n\nReviewed-on: https://forge.example/owner/repo/pulls/7",
        )])
        self.assertEqual(self.said[:3], [forge.merges[0][2], *self.lines])
        self.assertEqual(self.said[-1], f"landed #7 as {sha} with 2 Regression-Test lines")
        self.assertEqual(self.slept, [])
        self.assertEqual(list(landing.parse_regression_fields(forge.commit_message(sha))),
                         [line.split(": ", 1)[1] for line in self.lines])

    def test_the_trailers_are_what_landing_lines_prints(self):
        from validation.regression_field import main as regression_field

        body = ("Prose.\n\nRegression-Test: tests/operations/test_land.py"
                "::test_the_trailers_are_what_landing_lines_prints\n")
        out = io.StringIO()
        with mock.patch.dict("os.environ", {"PR_TITLE": "", "PR_BODY": "", "GITHUB_EVENT_PATH": ""}), \
                contextlib.redirect_stdout(out):
            code = regression_field(["--landing-lines", "--body", body])
        self.assertEqual(code, 0)
        lines = landing.landing_lines(landing.parse_regression_fields(body))
        self.assertEqual("".join(line + "\n" for line in lines), out.getvalue())
        _subject, message = landing.landing_message(pull(7, "fix(x): y", "0" * 40, body))
        self.assertEqual(landing.missing_trailers(message, lines), ())

    def test_a_pull_request_that_needs_no_line_lands_with_the_train_message_and_no_trailer(self):
        head = self.repo.branch("claude/docs", "docs: say more", {"notes/more.md": "more\n"})
        forge = FakeForge(self.repo, pull(8, "docs: say more", head, "Prose only.\n", "claude/docs"))
        self.assertEqual(self.land(forge, head), forge.landed)
        self.assertEqual(forge.merges, [(
            8, head, "Merge pull request 'docs: say more' (#8) from claude/docs into main",
            "Prose only.\n\nReviewed-on: https://forge.example/owner/repo/pulls/8",
        )])

    def test_a_dry_run_prints_the_message_and_merges_nothing(self):
        forge = self.forge()
        self.assertIsNone(self.land(forge, dry_run=True))
        self.assertEqual(forge.merges, [])
        self.assertEqual(self.said[:3], [
            "Merge pull request 'fix(thing): stop the thing' (#7) from claude/the-fix into main", *self.lines,
        ])
        self.assertIn("nothing was merged", self.said[-1])

    def test_try_again_later_is_waited_out_and_then_lands(self):
        forge = self.forge(answers=(later(), later(), None))
        self.assertEqual(self.land(forge, retries=5, wait=3.0), forge.landed)
        self.assertEqual(len(forge.merges), 3)
        self.assertEqual(self.slept, [3.0, 3.0])

    def test_try_again_later_is_bounded_and_says_nothing_merged(self):
        forge = self.forge(answers=tuple(later() for _ in range(10)))
        with self.assertRaises(landing.LandRefused) as raised:
            self.land(forge, retries=3, wait=2.0)
        self.assertEqual(len(forge.merges), 4)
        self.assertEqual(self.slept, [2.0, 2.0, 2.0])
        self.assertIn("still computing whether #7 merges", str(raised.exception))
        self.assertIn("nothing was merged", str(raised.exception))
        self.assertEqual(self.repo.git("rev-parse", "main"), self.repo.main)

    def test_a_forge_refusal_is_reported_and_a_conflict_is_called_one(self):
        for status, text, kind in (
            (405, "Does not have enough approvals", landing.LandRefused),
            (409, "head out of date", landing.LandRefused),
            (409, "There are merge conflicts", landing.LandConflict),
        ):
            with self.subTest(status=status, text=text):
                forge = self.forge(answers=(landing.ForgeError(status, json.dumps({"message": text})),))
                with self.assertRaises(landing.LandRefused) as raised:
                    self.land(forge)
                self.assertIs(type(raised.exception), kind)
                self.assertIn(text, str(raised.exception))
                self.assertEqual(self.slept, [])

    def test_a_landing_that_lost_its_trailers_is_an_error_naming_them(self):
        forge = self.forge(keep_message=False)
        with self.assertRaises(landing.LandedWrong) as raised:
            self.land(forge)
        why = str(raised.exception)
        self.assertIn(f"#7 landed as {forge.landed}", why)
        self.assertIn("it lacks `Regression-Test: tests/test_thing.py::test_the_thing_stops`", why)
        self.assertIn("[[errata]]", why)

    def test_the_lines_are_resolved_again_in_the_commit_that_actually_landed(self):
        forge = self.forge(lose_file="tests/test_thing.py")
        with self.assertRaises(landing.LandedWrong) as raised:
            self.land(forge)
        self.assertIn("the landing commit's own tree does not carry", str(raised.exception))

    def test_a_landing_commit_that_cannot_be_fetched_is_not_reported_as_good(self):
        forge = self.forge()

        def unreachable(_sha):
            raise landing.HistoryError("no such commit")

        with self.assertRaises(landing.LandedWrong) as raised:
            landing.land(forge, 7, self.head, self.repo.root, fetch=self.repo.fetch(self.head),
                         fetch_commit=unreachable, sleep=self.slept.append, say=self.said.append)
        self.assertIn("could not be fetched", str(raised.exception))

    # -- nothing moved between the checks and the merge ---------------------------

    def moving(self, change) -> FakeForge:
        """A forge on which `change` happens just before the pre-merge re-read."""

        def on_pull(forge: FakeForge) -> None:
            if forge.reads == 2:
                change(forge)

        return self.forge(on_pull=on_pull)

    def test_main_moving_between_the_checks_and_the_merge_is_refused(self):
        def main_moves(_forge):
            self.repo.write("later.txt", "later\n")
            self.repo.commit("chore: someone else landed")

        why = self.refused(self.moving(main_moves))
        self.assertIn("main moved from", why)
        self.assertIn("nothing was merged", why)

    def test_a_retarget_a_push_an_edit_or_a_redraft_before_the_merge_is_refused(self):
        for change, expected in (
            (lambda forge: forge.state.update(base={"ref": "effort/x"}), "only lands on main"),
            (lambda forge: forge.state.update(head={"ref": "claude/the-fix", "sha": "1" * 40}), "head is " + "1" * 40),
            (lambda forge: forge.state.update(body="Regression-Test: tests/test_thing.py::test_other\n"),
             "description was edited while landing"),
            (lambda forge: forge.state.update(title="WIP: " + self.TITLE), "still a draft"),
            (lambda forge: forge.state.update(state="closed"), "is closed"),
        ):
            with self.subTest(expected=expected):
                self.assertIn(expected, self.refused(self.moving(change)))

    # -- the message is the merge train's -----------------------------------------

    def test_the_real_body_of_1596_lands_as_the_train_would_with_its_six_raft_client_trailers(self):
        body = (FIXTURES / "pr-1596-body.md").read_text(encoding="utf-8")
        golden = (FIXTURES / "pr-1596-train-message.txt").read_text(encoding="utf-8")
        source = "".join(f"#[tokio::test]\nasync fn {name}() {{}}\n\n" for name in RAFT_TESTS)
        title = "fix(hiqlite): lease idle Raft client connections instead of dialing per client"
        branch = "fix/raft-client-reuse-idle-connections"
        head = self.repo.branch(branch, title, {RAFT_CLIENT: source})
        state = {**pull(1596, title, head, body, branch), "html_url": "http://forge.lan:3000/noirr/plurx/pulls/1596"}
        forge = FakeForge(self.repo, state)
        sha = self.land(forge, head)
        (_number, _head, subject, message), = forge.merges
        self.assertEqual(subject, f"Merge pull request '{title}' (#1596) from {branch} into main")
        # What ci-fleet 472831c's `_merge_message` builds for this pull request,
        # joined as its `_build` joins them for the commit it tests.
        self.assertEqual(subject + "\n\n" + message + "\n", golden)
        expected = [f"Regression-Test: {RAFT_CLIENT}::{name}" for name in RAFT_TESTS]
        self.assertEqual(self.said[1:7], expected)
        self.assertEqual([f"Regression-Test: {value}" for value in
                          landing.parse_regression_fields(forge.commit_message(sha))], expected)

    # -- the wire ----------------------------------------------------------------

    def test_the_merge_request_is_pinned_to_the_head_carries_the_message_and_deletes_the_branch(self):
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
            "delete_branch_after_merge": True,
        })

    def test_the_gate_is_read_from_the_commit_status_of_the_head(self):
        response = mock.MagicMock()
        response.__enter__.return_value = response
        response.read.return_value = json.dumps({"state": "success", "statuses": [{"context": GATE, "status": "skipped"}]}).encode()
        opener = mock.Mock(return_value=response)
        forge = landing.Forge("https://forge.example", "owner/repo", "test-token", opener)
        self.assertEqual(landing.gate_state(forge.statuses(self.head))[0], "skipped")
        self.assertEqual(opener.call_args.args[0].full_url,
                         f"https://forge.example/api/v1/repos/owner/repo/commits/{self.head}/status")

    def test_the_token_never_follows_a_redirect(self):
        seen: list[tuple[str, str | None]] = []

        def server(redirect_to: str | None):
            class Handler(http.server.BaseHTTPRequestHandler):
                def do_GET(self):
                    seen.append((self.path, self.headers.get("Authorization")))
                    if redirect_to:
                        self.send_response(302)
                        self.send_header("Location", redirect_to + self.path)
                        self.send_header("Content-Length", "0")
                        self.end_headers()
                    else:
                        payload = b'{"number": 7}'
                        self.send_response(200)
                        self.send_header("Content-Length", str(len(payload)))
                        self.end_headers()
                        self.wfile.write(payload)

                def log_message(self, *_args):
                    pass

            httpd = http.server.HTTPServer(("127.0.0.1", 0), Handler)
            threading.Thread(target=httpd.serve_forever, daemon=True).start()
            self.addCleanup(httpd.server_close)
            self.addCleanup(httpd.shutdown)
            return f"http://127.0.0.1:{httpd.server_address[1]}"

        elsewhere = server(None)
        forge = landing.Forge(server(elsewhere), "owner/repo", "sekrit-token")
        with self.assertRaises(landing.ForgeError) as raised:
            forge.pull(7)
        self.assertEqual(raised.exception.status, 302)
        self.assertEqual(seen, [("/api/v1/repos/owner/repo/pulls/7", "token sekrit-token")],
                         "only the configured forge may see the request")

    def test_the_command_line_wants_a_full_head_a_real_reason_and_never_prints_the_token(self):
        for argv, expected in (
            (["7", "--head", self.head[:9]], "full 40-character"),
            (["7", "--head", self.head, "--accept-untested", "  "], "needs the reason"),
        ):
            with self.subTest(argv=argv):
                err = io.StringIO()
                with mock.patch.dict("os.environ", {"FORGEJO_TOKEN": "sekrit-token"}), contextlib.redirect_stderr(err):
                    code = landing.main_land([*argv, "--root", str(self.repo.root)])
                self.assertEqual(code, 2)
                self.assertIn(expected, err.getvalue())
                self.assertNotIn("sekrit-token", err.getvalue())


class PushCheckCase(unittest.TestCase):
    """The job that runs on every push to main."""

    TEST = {"tests/test_thing.py": "def test_the_thing_stops():\n    assert True\n"}
    LINE = "Regression-Test: tests/test_thing.py::test_the_thing_stops"

    def setUp(self):
        self.repo = Repo(self)
        self.before = self.repo.main

    def merge(self, number: int, title: str, branch: str, files: dict[str, str], message: str = "") -> str:
        self.repo.branch(branch, title, files)
        subject = f"Merge pull request '{title}' (#{number}) from {branch} into main"
        self.repo.git("merge", "-q", "--no-ff", branch, "-m", subject + ("\n\n" + message if message else ""))
        return self.repo.git("rev-parse", "HEAD")

    def errors(self, pulls: dict | None = None) -> list[str]:
        """Offline when `pulls` is None; otherwise the forge's answer per number."""

        if pulls is None:
            return landing.audit_push(self.repo.root, self.before, "HEAD", None)

        def pull_of(number: int):
            answer = pulls.get(number)
            if isinstance(answer, Exception):
                raise answer
            return answer

        return landing.audit_push(self.repo.root, self.before, "HEAD", pull_of)

    def test_a_landing_carrying_its_lines_passes(self):
        sha = self.merge(7, "fix(thing): stop the thing", "b", self.TEST, f"Words.\n\n{self.LINE}\n\nReviewed-on: u")
        self.assertEqual(self.errors({7: {"merge_commit_sha": sha, "body": f"Words.\n\n{self.LINE}\n"}}), [])
        self.assertEqual(self.errors(None), [])

    def test_a_corrective_landing_with_the_title_alone_fails_naming_the_pull_request(self):
        sha = self.merge(7, "fix(thing): stop the thing", "b", self.TEST)
        online = {7: {"merge_commit_sha": sha, "body": f"Words.\n\n{self.LINE}\n"}}
        for pulls in (None, online):
            with self.subTest(pulls=pulls):
                errors = self.errors(pulls)
                self.assertTrue(errors)
                self.assertTrue(all(error.startswith(f"#7 landed as {sha[:9]}") for error in errors), errors)
        self.assertIn(self.LINE, "\n".join(self.errors(online)))

    def test_a_feature_landing_that_dropped_its_description_lines_fails(self):
        sha = self.merge(9, "feat(thing): more thing", "b", self.TEST)
        self.assertEqual(self.errors(None), [])
        errors = self.errors({9: {"merge_commit_sha": sha, "body": f"{self.LINE}\n"}})
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

    def test_an_unreadable_forge_fails_a_corrective_landing_and_not_a_plain_one(self):
        self.merge(7, "fix(thing): stop the thing", "b", self.TEST, self.LINE)
        self.merge(8, "docs: words", "c", {"c.txt": "c\n"})
        errors = self.errors({7: landing.Unreadable("HTTP 502"), 8: landing.Unreadable("HTTP 502")})
        self.assertEqual(len(errors), 1)
        self.assertIn("#7 landed", errors[0])
        self.assertIn("could not be read from the forge (HTTP 502)", errors[0])

    def test_a_subject_that_only_cites_a_number_is_not_compared_with_that_pull_request(self):
        self.repo.write("notes.md", "n\n")
        self.repo.commit("docs: explain the thing (#12)")
        cited = {"merge_commit_sha": "e" * 40, "body": f"{self.LINE}\n"}
        for pulls in ({12: None}, {12: cited}, None):
            with self.subTest(pulls=pulls):
                self.assertEqual(self.errors(pulls), [])

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
                self.assertEqual(len(landing.audit_push(self.repo.root, before, "HEAD", None)), 1)

    def run_check(self, *argv: str, env: dict | None = None) -> tuple[int, str, str]:
        clean = {"GITHUB_API_URL": "", "GITHUB_REPOSITORY": "", "GITHUB_TOKEN": "",
                 "LANDING_BEFORE": "", "LANDING_SHA": "", **(env or {})}
        out, err = io.StringIO(), io.StringIO()
        with mock.patch.dict("os.environ", clean), contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = landing.main_check(["--root", str(self.repo.root), *argv])
        return code, out.getvalue(), err.getvalue()

    def test_the_command_fails_loudly_and_passes_quietly(self):
        self.merge(7, "fix(thing): stop the thing", "b", self.TEST)
        code, out, err = self.run_check("--offline", "--before", self.before)
        self.assertEqual(code, 1)
        self.assertIn("::error::#7 landed as", out)
        self.assertIn("LANDING WITHOUT ITS REGRESSION-TEST LINES", err)
        self.assertIn("scripts/land", err)
        code, out, _err = self.run_check("--offline", "--before", "HEAD")
        self.assertEqual(code, 0)
        self.assertIn("landing check ok", out)

    def test_the_command_without_a_forge_is_an_error_not_a_pass(self):
        self.merge(7, "fix(thing): stop the thing", "b", self.TEST, self.LINE)
        code, _out, err = self.run_check("--before", self.before)
        self.assertEqual(code, 2)
        self.assertIn("no forge API in this environment", err)

    def test_the_command_fails_closed_when_the_forge_does_not_answer(self):
        self.merge(7, "fix(thing): stop the thing", "b", self.TEST, self.LINE)
        env = {"GITHUB_API_URL": "https://forge.example/api/v1", "GITHUB_REPOSITORY": "owner/repo"}
        with mock.patch.object(landing.Forge, "pull", side_effect=landing.ForgeError(502, "bad gateway")):
            code, out, _err = self.run_check("--before", self.before, env=env)
        self.assertEqual(code, 1)
        self.assertIn("could not be read from the forge (HTTP 502)", out)
        with mock.patch.object(landing.Forge, "pull", side_effect=landing.ForgeError(404, "not found")):
            code, _out, _err = self.run_check("--before", self.before, env=env)
        self.assertEqual(code, 0)


class WiringCase(unittest.TestCase):
    def test_the_push_to_main_workflow_runs_the_landing_check_over_the_whole_push(self):
        workflow = (ROOT / ".github/workflows/landing-check.yml").read_text(encoding="utf-8")
        self.assertRegex(workflow, r"(?m)^on:\n  push:\n    branches: \[main\]\n")
        self.assertIn("fetch-depth: 0", workflow)
        self.assertIn("GITHUB_TOKEN: ${{ github.token }}", workflow)
        self.assertIn("LANDING_BEFORE: ${{ github.event.before }}", workflow)
        self.assertIn("LANDING_SHA: ${{ github.sha }}", workflow)
        self.assertIn("python3 -m validation.landing check", workflow)
        self.assertNotIn("--offline", workflow)

    def test_scripts_land_is_the_documented_off_mode_path(self):
        script = ROOT / "scripts/land"
        self.assertTrue(script.stat().st_mode & 0o111, "scripts/land must be executable")
        self.assertIn("main_land", script.read_text(encoding="utf-8"))
        for doc in ("AGENTS.md", "docs/ci/MERGE-TRAIN.md"):
            text = " ".join((ROOT / doc).read_text(encoding="utf-8").split())
            self.assertTrue("scripts/land <pr> --head <sha>" in text, doc)
            self.assertTrue("never the raw merge API" in text, doc)
        agents = (ROOT / "AGENTS.md").read_text(encoding="utf-8")
        self.assertTrue("full 40-character" in " ".join(agents.split()))
        self.assertTrue("exits 3" in " ".join(agents.split()))
        self.assertFalse([line for line in agents.splitlines() if "scripts/land" in line and len(line) > 100])


if __name__ == "__main__":
    unittest.main()
