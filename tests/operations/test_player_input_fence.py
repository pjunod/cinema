"""The player input fence trips on handlers and not on prose about them.

Two Live TV doc comments naming `onMoveCommand` failed this fence on `main`
with no key handler anywhere near them. A fence that fails on prose gets
worked around — but an exemption that widens is worse than the failure it
fixed, so the cases below are the ones that must keep tripping: a comment that
closes and then runs code (`/* named = */ value` is already an idiom in this
tree), a `*/` that ends a block and then calls something, a JS generator
method, a private class field, and an Objective-C macro. None of those open a
comment in any language this fence scans.
"""

from __future__ import annotations

import importlib.machinery
import importlib.util
from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[2]
FIXTURES = ROOT / "tests/operations/fence-fixtures/input"


def load_fence():
    loader = importlib.machinery.SourceFileLoader("player_input_fence", str(ROOT / "scripts/player-input-fence"))
    spec = importlib.util.spec_from_loader(loader.name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


# Executable lines the exemption must never swallow. Each one carries a token
# the fence exists to catch.
MUST_NOT_SKIP = (
    '/* not a handler */ .onMoveCommand { d in move(d) }',
    ' */ .onExitCommand { dismiss() }',
    '<!-- panel --><div onkeydown="handleKey(event)"></div>',
    '  *keys(ev) { return ev.key; }',
    '  #onkeydown = (e) => e.key;',
    '#define PLURX_KEY [UIKeyCommand keyCommandWithInput:@"x"]',
    '/* named = */ onKeyEvent { it.key == Key.DirectionLeft }',
    '        .onMoveCommand { d in move(d) } // onExitCommand is elsewhere',
    '        .onExitCommand { dismiss() } /// see onMoveCommand above',
)

# Lines that really are nothing but comment.
MUST_SKIP = (
    '/// `onMoveCommand` consumes every direction it is given.',
    '// onExitCommand is the engine\'s, not ours.',
    '/* pressesBegan is documented elsewhere. */',
    ' * onPlayPauseCommand lives in PlayerRemoteAdapter.',
    ' */',
    '<!-- onkeydown= is described in the reader docs -->',
)


class PlayerInputFenceTest(unittest.TestCase):
    def setUp(self):
        self.fence = load_fence()

    def scan(self, text: str, relative=Path("clients/apple/Sources/Fixture.swift")):
        return self.fence.scan_lines(relative, text.splitlines(), set())

    def flagged_lines(self, text: str) -> set[str]:
        """The distinct line numbers the fence flagged. One line can name two
        tokens (`onKeyEvent { it.key == Key.Left }` is both), and the fence
        reports each; what matters here is which lines it saw at all."""
        return {failure.split(":")[1] for failure in self.scan(text)}

    def test_a_real_handler_still_trips(self):
        text = (FIXTURES / "must_trip.swift").read_text(encoding="utf-8")
        self.assertEqual(len(self.flagged_lines(text)), 4, f"the fence stopped seeing handlers: {self.scan(text)}")

    def test_prose_about_a_handler_does_not(self):
        found = self.scan((FIXTURES / "must_not_trip.swift").read_text(encoding="utf-8"))
        self.assertEqual(found, [], f"the fence tripped on a doc comment: {found}")

    def test_the_exemption_does_not_widen(self):
        for line in MUST_NOT_SKIP:
            with self.subTest(line=line):
                self.assertFalse(
                    self.fence.is_comment_line(line),
                    "an executable line was treated as a comment",
                )
                self.assertEqual(len(self.flagged_lines(line)), 1, "the fence stopped seeing this handler")

    def test_real_comments_are_still_skipped(self):
        for line in MUST_SKIP:
            with self.subTest(line=line):
                self.assertTrue(self.fence.is_comment_line(line))
                self.assertEqual(self.scan(line), [])

    def test_an_allowed_region_silences_only_its_own_lines(self):
        text = (FIXTURES / "must_trip.swift").read_text(encoding="utf-8")
        lines = text.splitlines()
        relative = Path("clients/apple/Sources/Fixture.swift")
        everything = set(range(1, len(lines) + 1))
        self.assertEqual(self.fence.scan_lines(relative, lines, everything), [])

    def test_scan_lines_honours_the_token_file_allowlist(self):
        token, relative = next(iter(self.fence.TOKEN_FILE_ALLOWLIST))
        self.assertEqual(token, "MPRemoteCommandCenter")
        self.assertEqual(self.fence.scan_lines(relative, ["let c = MPRemoteCommandCenter.shared()"], set()), [])
        other = Path("clients/apple/Sources/PlayerView.swift")
        self.assertEqual(len(self.fence.scan_lines(other, ["let c = MPRemoteCommandCenter.shared()"], set())), 1)


    def test_the_fence_passes_the_repository_as_it_stands(self):
        self.assertEqual(self.fence.scan(), [])


if __name__ == "__main__":
    unittest.main()
