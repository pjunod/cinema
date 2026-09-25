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
        allowed = self.fence.TOKEN_FILE_ALLOWLIST
        self.assertEqual(
            {relative for token, relative in allowed if token == "MPRemoteCommandCenter"},
            {
                Path("clients/apple/Sources/PlayerController.swift"),
                Path("clients/apple/Sources/RemoteCommandOwner.swift"),
            },
        )
        for token, relative in allowed:
            with self.subTest(token=token, relative=relative):
                self.assertEqual(token, "MPRemoteCommandCenter")
                self.assertEqual(
                    self.fence.scan_lines(relative, ["let c = MPRemoteCommandCenter.shared()"], set()),
                    [],
                )
        other = Path("clients/apple/Sources/PlayerView.swift")
        self.assertEqual(len(self.fence.scan_lines(other, ["let c = MPRemoteCommandCenter.shared()"], set())), 1)


    def test_every_web_row_is_scanned_and_a_handler_in_one_trips(self):
        # The web app is a tree now (docs/clients/WEB-SHELL-LAYOUT.md). This
        # fence used to name `index.html` and glob the flat `web/*.js`, which
        # after the split reached none of the sixty rows the app actually lives
        # in — a `keydown` listener in any of them would have been invisible.
        scanned = set(self.fence.client_files())
        rows = sorted((ROOT / "crates/plurxd/src/web").rglob("*.js"))
        for row in rows:
            if row.name.endswith(".min.js"):
                continue
            self.assertIn(
                row.relative_to(ROOT), scanned,
                f"{row.relative_to(ROOT)} is a shipped web script the fence does not scan",
            )
        handler = 'window.addEventListener("keydown", (ev) => { if (ev.key === "Escape") close(); });'
        for name in ("pages/settings.js", "player/menus.js", "core/cards.js", "router.js"):
            with self.subTest(row=name):
                relative = Path("crates/plurxd/src/web") / name
                self.assertEqual(
                    len(self.fence.scan_lines(relative, [handler], set())), 2,
                    f"a key handler added to {name} was not seen",
                )

    def test_each_web_region_names_the_file_it_lives_in(self):
        # Scoping the regions per file is what stops an anchor that moves to
        # another row from silently widening its region to everything between
        # the two. Each anchor must appear exactly once, in the file named.
        for path, start_text, end_text, reason in self.fence.WEB_REGIONS:
            with self.subTest(region=start_text):
                self.assertTrue(reason.strip(), f"{start_text} is allowed with no reason")
                lines = (ROOT / path).read_text(encoding="utf-8").splitlines()
                self.assertEqual(
                    sum(1 for line in lines if start_text in line), 1,
                    f"{path} does not carry {start_text!r} exactly once",
                )
                if end_text is not None:
                    self.assertIn(end_text, "\n".join(lines), f"{path} lost {end_text!r}")
        # …and a lost anchor is an error, not an empty allowlist nobody notices.
        path, start_text, *_rest = self.fence.WEB_REGIONS[0]
        with self.assertRaises(ValueError):
            self.fence.web_allowed_lines(path, ["nothing();"])

    def test_the_fence_passes_the_repository_as_it_stands(self):
        self.assertEqual(self.fence.scan(), [])


if __name__ == "__main__":
    unittest.main()
