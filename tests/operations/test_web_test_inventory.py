"""Every Node web test is run by a lane, and the list cannot rot silently.

`make web-unit-check` names its tests one per line. Eight `*.test.js` files
were committed beside it and never added, so the regressions they pin ran
nowhere: not locally, not in a pull request, not in the release sweep. This
suite is in `make operations-check`, which every pull request's preflight runs,
so a test file nothing executes fails the pull request that adds it.

Only COMMANDS a lane runs count. For Node tests that is the `web-unit-check`
recipe plus the `node` commands of the fast lane's web job, which runs that
target; `make web-check` runs in no lane, so naming a test only there runs it
nowhere. For browser fixtures it is any command of a workflow, or of a Make
target a workflow runs (with its prerequisites and nested `$(MAKE)` calls). A
file named in a Makefile comment, in a `validation/points.toml` command no lane
is guaranteed to select, or in a target no workflow invokes is not executed by
naming it there.
"""

from __future__ import annotations

from pathlib import Path
import re
import unittest


ROOT = Path(__file__).resolve().parents[2]
NODE_TEST_DIRS = ("tests/web", "tests/playback")

# Real-browser fixtures no lane runs yet. This set may only shrink: wire one
# into a Make target (or delete it) and remove its row. A new `*.browser.cjs`
# must arrive with a target that runs it.
UNGATED_BROWSER_FIXTURES = {
    "tests/web/durable-work.browser.cjs",
    "tests/web/dvr-ui.browser.cjs",
    "tests/web/hls-seek.browser.cjs",
    "tests/web/library-layout.browser.cjs",
    "tests/web/live-tv-logos.browser.cjs",
    "tests/web/live-tv-startup.browser.cjs",
    # `make media-preparation-browser-check` runs it, and no workflow runs
    # that target.
    "tests/web/media-preparation.browser.cjs",
    "tests/web/watch-and-browse.browser.cjs",
}
FAST_LANE = ".github/workflows/main-fast-lane.yml"
WEB_JOB = "web_compile"

# Make's recipe prefixes: `@` silences, `-` ignores errors, `+` forces. Make
# accepts blanks between and after them (`- @ cmd`, `@ # note`).
RECIPE_PREFIX = re.compile(r"^[@+\-\s]*")


def command_lines(recipe: str) -> list[str]:
    """A recipe's commands: tab-indented lines that are not comments."""
    commands = []
    for line in recipe.splitlines():
        if not line.startswith("\t"):
            continue
        # `@ # note` is a comment too: strip the prefixes, then the blanks
        # between them and the shell text.
        body = RECIPE_PREFIX.sub("", line.lstrip("\t").lstrip()).lstrip()
        if body.startswith("#") or not body.strip():
            continue
        commands.append(body)
    return commands


def target_rule(makefile: str, target: str) -> re.Match[str] | None:
    return re.search(
        rf"^{re.escape(target)}:([^\n]*)\n((?:\t[^\n]*\n|\n(?=\t))*)", makefile, re.M
    )


def target_recipe(makefile: str, target: str) -> str:
    """The lines of one target's recipe, up to the next rule or blank gap."""
    match = target_rule(makefile, target)
    assert match, f"Makefile has no {target} target"
    return match.group(2)


def target_prerequisites(makefile: str, target: str) -> list[str]:
    match = target_rule(makefile, target)
    if match is None:
        return []
    names = match.group(1).split("#", 1)[0].replace("|", " ").split()
    return [name for name in names if re.fullmatch(r"[a-z][\w.-]*", name)]


def uncommented(text: str) -> list[str]:
    return [line for line in text.splitlines() if not line.lstrip().startswith("#")]


def workflow_commands() -> str:
    """Every non-comment line of every workflow and local action."""
    files = sorted((ROOT / ".github" / "workflows").glob("*.yml")) + sorted(
        (ROOT / ".github" / "actions").glob("**/*.yml")
    )
    return "\n".join(
        line for path in files for line in uncommented(path.read_text(encoding="utf-8"))
    )


MAKE_CALL = re.compile(r"(?:\bmake|\$\(MAKE\))((?:\s+-\S+)*)\s+([a-z][\w.-]*)")


def lane_recipe_commands() -> list[str]:
    """The commands of every Make target a workflow runs, transitively."""
    makefile = makefile_text()
    pending = [match.group(2) for match in MAKE_CALL.finditer(workflow_commands())]
    seen: set[str] = set()
    commands: list[str] = []
    while pending:
        target = pending.pop()
        if target in seen:
            continue
        seen.add(target)
        rule = target_rule(makefile, target)
        if rule is None:
            continue
        pending.extend(target_prerequisites(makefile, target))
        for command in command_lines(rule.group(2)):
            commands.append(command)
            pending.extend(match.group(2) for match in MAKE_CALL.finditer(command))
    return commands


def fast_lane_web_job() -> str:
    workflow = (ROOT / FAST_LANE).read_text(encoding="utf-8")
    match = re.search(rf"(?ms)^  {WEB_JOB}:\n(.*?)(?=^  [a-z_]+:\n|\Z)", workflow)
    assert match, f"{FAST_LANE} has no {WEB_JOB} job"
    return match.group(1)


def makefile_text() -> str:
    return (ROOT / "Makefile").read_text(encoding="utf-8")


def web_commands() -> list[str]:
    makefile = makefile_text()
    return command_lines(target_recipe(makefile, "web-unit-check")) + command_lines(
        target_recipe(makefile, "web-check")
    )


def lane_node_test_commands() -> list[str]:
    """What a pull request executes: web-unit-check, which the fast lane's web
    job runs, plus that job's own `node` lines."""
    job = fast_lane_web_job()
    assert re.search(r"(?m)^\s+run: make web-unit-check\s*$", job), (
        f"{FAST_LANE} {WEB_JOB} no longer runs make web-unit-check"
    )
    own = [
        re.sub(r"^run:\s*", "", line.strip())
        for line in uncommented(job)
        if re.search(r"\bnode ", line)
    ]
    return command_lines(target_recipe(makefile_text(), "web-unit-check")) + own


def required_by(executed: set[str]) -> set[str]:
    """Test files an executed test file loads with `require("./x.test.js")`."""
    found = set()
    for path in executed:
        body = (ROOT / path).read_text(encoding="utf-8")
        for name in re.findall(r"""require\(["']\./([\w.-]+\.test\.js)["']\)""", body):
            found.add(str(Path(path).parent / name))
    return found


def present(glob: str) -> set[str]:
    return {
        str(path.relative_to(ROOT))
        for directory in NODE_TEST_DIRS
        for path in (ROOT / directory).glob(glob)
    }


class WebTestInventory(unittest.TestCase):
    def test_comment_and_prefix_parsing(self):
        recipe = (
            "\t@# tests/web/a.test.js is named in a comment\n"
            "\t# tests/web/b.test.js too\n"
            "\t@node tests/web/c.test.js\n"
            "\t-@node --test tests/web/d.test.js\n"
            "\t@ # node tests/web/e.test.js, a comment after a prefix and a blank\n"
            "\t- @ node tests/web/f.test.js\n"
        )
        self.assertEqual(
            command_lines(recipe),
            [
                "node tests/web/c.test.js",
                "node --test tests/web/d.test.js",
                "node tests/web/f.test.js",
            ],
        )

    def test_every_node_test_file_is_executed_by_a_gate(self):
        tests = present("*.test.js")
        self.assertGreater(len(tests), 40, "the globs stopped finding the suites")
        commands = "\n".join(lane_node_test_commands())
        named = {
            path
            for path in tests
            if re.search(rf"\bnode (?:--test )?(?:\S+ )*{re.escape(path)}(?:\s|$)", commands)
        }
        executed = named | (required_by(named) & tests)
        self.assertEqual(
            sorted(tests - executed),
            [],
            "these test files exist and no lane runs them; add each to the "
            "web-unit-check recipe in the Makefile (web-check alone runs in no lane)",
        )

    def test_the_recipe_names_no_test_file_that_is_gone(self):
        for path in re.findall(r"tests/\S+\.(?:test\.js|browser\.cjs)", "\n".join(web_commands())):
            self.assertTrue((ROOT / path).is_file(), f"web-check runs {path}, which does not exist")

    def test_web_check_alone_is_not_a_lane(self):
        # The rule this file exists for: a test named only in web-check runs
        # in no pull request, so it does not count as executed.
        self.assertNotIn("make web-check", workflow_commands())
        lane = "\n".join(lane_node_test_commands())
        self.assertNotIn("scripts/contrast-check", lane)
        self.assertIn("node tests/web/player-typedef.test.js", lane)
        self.assertIn("node tests/playback/web-control.test.js", lane)

    def test_lane_targets_follow_prerequisites_and_nested_make(self):
        reached = "\n".join(lane_recipe_commands())
        # operations-check runs from every preflight; its recipe is reached.
        self.assertIn("unittest discover -s tests/operations", reached)
        # web-unit-check is reached through the fast lane's web job.
        self.assertIn("node tests/playback/web-control.test.js", reached)
        # A target nothing invokes is not.
        self.assertNotIn("tests/web/media-preparation.browser.cjs", reached)

    def test_ungated_browser_fixtures_only_shrink(self):
        commands = workflow_commands() + "\n" + "\n".join(lane_recipe_commands())
        fixtures = present("*.browser.cjs")
        ungated = {path for path in fixtures if path not in commands}
        self.assertEqual(
            sorted(ungated - UNGATED_BROWSER_FIXTURES),
            [],
            "a new browser fixture needs a lane that runs it (a workflow command, "
            "or a Make target a workflow invokes)",
        )
        self.assertEqual(
            sorted(UNGATED_BROWSER_FIXTURES - ungated),
            [],
            "these are gated or gone now; remove them from UNGATED_BROWSER_FIXTURES",
        )


if __name__ == "__main__":
    unittest.main()
