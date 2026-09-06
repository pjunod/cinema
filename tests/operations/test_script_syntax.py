"""Every Python script in `scripts/` has to parse.

`scripts/live-tv-browser` shipped with a `SyntaxError` -- one `print` dedented
out of its block -- and nothing noticed until `make ui-check` invoked it on CI,
after a 446-second browser sweep that passed. Nothing else in the tree reads
these files, so nothing else would ever have said so.

This compiles rather than imports: no module is executed, no dependency is
required, and a script that needs Playwright or a running daemon is judged the
same as one that needs nothing. That is deliberately a low bar. It is also the
bar `live-tv-browser` failed.
"""

from __future__ import annotations

import ast
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPTS = ROOT / "scripts"


def python_scripts() -> list[Path]:
    """Every script whose shebang says Python, plus every `.py`.

    Extension is not the signal here: almost all of these are extensionless
    executables, and the shebang is what makes them Python.
    """

    found = []
    for path in sorted(SCRIPTS.rglob("*")):
        if not path.is_file():
            continue
        if path.suffix == ".py":
            found.append(path)
            continue
        try:
            with path.open("rb") as handle:
                first = handle.readline()
        except OSError:
            continue
        if first.startswith(b"#!") and b"python" in first:
            found.append(path)
    return found


class ScriptSyntaxCase(unittest.TestCase):
    def test_every_python_script_parses(self):
        scripts = python_scripts()
        self.assertGreater(
            len(scripts), 15, "found almost no scripts; the scanner is broken, not the tree"
        )
        broken = []
        for path in scripts:
            source = path.read_text(encoding="utf-8")
            try:
                ast.parse(source, filename=str(path))
            except SyntaxError as error:
                broken.append(
                    f"{path.relative_to(ROOT)}:{error.lineno}: {error.msg}"
                )
        self.assertEqual(broken, [], "these scripts do not parse:\n" + "\n".join(broken))


if __name__ == "__main__":
    unittest.main()
