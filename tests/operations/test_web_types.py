"""The web type-check ratchet fails on what rose and on nothing else.

`scripts/web-types` runs `tsc` over the web shell and compares per-file,
per-code diagnostic counts with `tests/web/tsc-baseline.tsv`
(docs/clients/WEB-TYPE-CHECKING-AND-PLAYER-DECOMPOSITION.md §3.3). These
cases feed it a recorded tsc run so the ratchet itself is pinned: a new key
or a higher count fails and names the line; a lower count passes; `--update`
only lowers; `--accept-increase` records its reason; and output tsc prints
for a broken config is a failure, not an empty — and therefore passing — run.
"""

from __future__ import annotations

import contextlib
import importlib.machinery
import importlib.util
import io
from pathlib import Path
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]


def load():
    loader = importlib.machinery.SourceFileLoader("web_types", str(ROOT / "scripts/web-types"))
    spec = importlib.util.spec_from_loader(loader.name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


WT = load()

A = "crates/plurxd/src/web/core/cards.js"
B = "crates/plurxd/src/web/player/stats.js"

TWO_IN_A = (
    f"{A}(10,5): error TS2339: Property 'x' does not exist on type 'HTMLElement'.\n"
    f"{A}(20,7): error TS2339: Property 'y' does not exist on type 'HTMLElement'.\n"
)
ONE_IN_B = (
    f"{B}(3,1): error TS2322: Type '{{ a: number; }}' is not assignable to type 'HeadersInit'.\n"
    "  Type '{ a: number; }' is not assignable to type 'Record<string, string>'.\n"
)


class Ratchet(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.dir.cleanup)
        self.base = Path(self.dir.name) / "baseline.tsv"
        self.out = Path(self.dir.name) / "tsc.out"

    def run_gate(self, output: str, *extra: str):
        self.out.write_text(output, encoding="utf-8")
        stdout, stderr = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            code = WT.main(["--baseline", str(self.base), "--tsc-output", str(self.out), *extra])
        return code, stdout.getvalue(), stderr.getvalue()

    def seed(self, output: str):
        code, _, err = self.run_gate(output, "--accept-increase", "seed")
        self.assertEqual(code, 0, err)

    def test_unchanged_passes(self):
        self.seed(TWO_IN_A + ONE_IN_B)
        code, out, _ = self.run_gate(TWO_IN_A + ONE_IN_B)
        self.assertEqual(code, 0)
        self.assertIn("unchanged", out)

    def test_a_new_key_fails_and_names_the_line(self):
        self.seed(TWO_IN_A)
        code, _, err = self.run_gate(
            TWO_IN_A + f"{A}(42,3): error TS2304: Cannot find name 'undefinedFn'.\n")
        self.assertEqual(code, 1)
        self.assertIn(f"{A}:42:3 TS2304 Cannot find name 'undefinedFn'.", err)

    def test_a_higher_count_under_an_existing_key_fails(self):
        self.seed(TWO_IN_A)
        code, _, err = self.run_gate(
            TWO_IN_A + f"{A}(30,1): error TS2339: Property 'z' does not exist on type 'Element'.\n")
        self.assertEqual(code, 1)
        self.assertIn("TS2339: 3 now, baseline allows 2", err)

    def test_a_lower_count_passes_and_says_so(self):
        self.seed(TWO_IN_A + ONE_IN_B)
        code, out, _ = self.run_gate(TWO_IN_A)
        self.assertEqual(code, 0)
        self.assertIn("can shrink by 1", out)

    def test_update_writes_nothing_when_nothing_shrank(self):
        self.seed(TWO_IN_A)
        before = self.base.read_text(encoding="utf-8")
        code, out, _ = self.run_gate(TWO_IN_A, "--update")
        self.assertEqual(code, 0)
        self.assertIn("nothing shrank", out)
        self.assertEqual(self.base.read_text(encoding="utf-8"), before)

    def test_update_lowers_and_drops_emptied_keys(self):
        self.seed(TWO_IN_A + ONE_IN_B)
        code, _, _ = self.run_gate(TWO_IN_A.splitlines(keepends=True)[0], "--update")
        self.assertEqual(code, 0)
        counts, accepted = WT.read_baseline(self.base)
        self.assertEqual(dict(counts), {(A, "TS2339"): 1})
        self.assertEqual(len(accepted), 1, "the seed's accepted line must survive an update")

    def test_update_refuses_to_raise(self):
        self.seed(ONE_IN_B)
        before = self.base.read_text(encoding="utf-8")
        code, _, err = self.run_gate(TWO_IN_A + ONE_IN_B, "--update")
        self.assertEqual(code, 1)
        self.assertIn("accept-increase", err)
        self.assertEqual(self.base.read_text(encoding="utf-8"), before)

    def test_accept_increase_records_the_reason(self):
        self.seed(ONE_IN_B)
        code, _, _ = self.run_gate(TWO_IN_A + ONE_IN_B, "--accept-increase", "  a  typed   field ")
        self.assertEqual(code, 0)
        text = self.base.read_text(encoding="utf-8")
        self.assertRegex(text, r"# accepted \d{4}-\d{2}-\d{2}: a typed field\n")
        self.assertIn(f"{A}\tTS2339\t2\n", text)

    def test_accept_increase_needs_a_reason(self):
        code, _, _ = self.run_gate(TWO_IN_A, "--accept-increase", "   ")
        self.assertEqual(code, 2)
        self.assertFalse(self.base.exists())

    def test_output_that_is_not_a_file_diagnostic_fails(self):
        # A broken jsconfig.json makes tsc print a config error and check
        # nothing. Counting that as zero diagnostics would pass the gate.
        self.seed(TWO_IN_A)
        code, _, err = self.run_gate("error TS5023: Unknown compiler option 'nope'.\n")
        self.assertEqual(code, 1)
        self.assertIn("not a file diagnostic", err)

    def test_a_continuation_line_belongs_to_its_diagnostic(self):
        diagnostics, stray = WT.parse(ONE_IN_B)
        self.assertEqual(stray, [])
        self.assertEqual(len(diagnostics), 1)
        self.assertIn("Record<string, string>", diagnostics[0]["message"])


class Pinned(unittest.TestCase):
    def test_typescript_is_pinned_exactly_and_dev_only(self):
        import json
        manifest = json.loads((ROOT / "tools/web-types/package.json").read_text(encoding="utf-8"))
        self.assertEqual(manifest.get("dependencies"), None, "tsc is a dev tool, never a dependency")
        wanted = manifest["devDependencies"]["typescript"]
        self.assertRegex(wanted, r"^\d+\.\d+\.\d+$", "pin an exact version, not a range")
        self.assertEqual(WT.locked_version(), wanted, "package-lock.json disagrees with package.json")


if __name__ == "__main__":
    unittest.main()
