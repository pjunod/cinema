"""The player input fence trips on handlers and not on prose about them.

Two Live TV doc comments naming `onMoveCommand` failed this fence on `main`
with no key handler anywhere near them. A fence that fails on prose gets
worked around, so comment lines are skipped — and these fixtures are what keep
that exemption from quietly becoming "skip anything with a slash in it".
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


def hits(fence, text: str) -> list[tuple[int, str]]:
    found = []
    for number, line in enumerate(text.splitlines(), start=1):
        if fence.is_comment_line(line):
            continue
        for token, pattern in fence.TOKENS:
            if pattern.search(line):
                found.append((number, token))
                break
    return found


class PlayerInputFenceTest(unittest.TestCase):
    def setUp(self):
        self.fence = load_fence()

    def test_a_real_handler_still_trips(self):
        found = hits(self.fence, (FIXTURES / "must_trip.swift").read_text(encoding="utf-8"))
        self.assertEqual(len(found), 2, f"the fence stopped seeing handlers: {found}")

    def test_prose_about_a_handler_does_not(self):
        found = hits(self.fence, (FIXTURES / "must_not_trip.swift").read_text(encoding="utf-8"))
        self.assertEqual(found, [], "the fence tripped on a doc comment")

    def test_a_trailing_comment_never_hides_code(self):
        line = "        .onMoveCommand { d in move(d) } // onExitCommand is elsewhere"
        self.assertFalse(self.fence.is_comment_line(line))
        self.assertEqual(len(hits(self.fence, line)), 1)

    def test_the_fence_passes_the_repository_as_it_stands(self):
        self.assertEqual(self.fence.scan(), [])


if __name__ == "__main__":
    unittest.main()
