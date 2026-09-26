"""The web type-check ratchet fails on what rose and on nothing else.

`scripts/web-types` runs `tsc` over the web shell and compares per-file,
per-code diagnostic counts with `tests/web/tsc-baseline.tsv`
(docs/clients/WEB-TYPE-CHECKING-AND-PLAYER-DECOMPOSITION.md §3.3). These
cases feed it a recorded tsc run so the ratchet itself is pinned: a new key
or a higher count fails and names the line; a lower count fails as a stale
baseline until `--update` lowers it, so headroom cannot hide a later error
under the same key; `--update` only lowers; `--accept-increase` records one
reason line per raised key, and `--base` refuses a raise that has none; and
output tsc prints for a broken config is a failure, not an empty — and
therefore passing — run.
"""

from __future__ import annotations

import contextlib
import importlib.machinery
import importlib.util
import io
from pathlib import Path
import subprocess
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

    def test_a_lower_count_fails_until_the_baseline_is_lowered(self):
        self.seed(TWO_IN_A + ONE_IN_B)
        code, _, err = self.run_gate(TWO_IN_A)
        self.assertEqual(code, 1)
        self.assertIn(f"{B} TS2322: 0 now, baseline says 1", err)
        self.assertIn("baseline stale", err)
        self.assertEqual(self.run_gate(TWO_IN_A, "--update")[0], 0)
        self.assertEqual(self.run_gate(TWO_IN_A)[0], 0)

    def test_headroom_from_a_removed_error_cannot_hide_a_new_one(self):
        # PR #554 review, finding 2: one TS2339 removed, then a misspelled
        # field added under the same key in a later change. When a fall only
        # printed "can shrink", the second run read "unchanged" and passed.
        self.seed(TWO_IN_A)
        one_left = TWO_IN_A.splitlines(keepends=True)[0]
        self.assertEqual(self.run_gate(one_left)[0], 1, "the fall itself must fail")
        self.assertEqual(self.run_gate(one_left, "--update")[0], 0)
        misspelt = one_left + f"{A}(404,61): error TS2339: Property 'sourceFilename' does not exist on type 'Player'.\n"
        code, _, err = self.run_gate(misspelt)
        self.assertEqual(code, 1)
        self.assertIn(f"{A}:404:61 TS2339 Property 'sourceFilename'", err)

    def test_update_writes_nothing_when_nothing_shrank(self):
        self.seed(TWO_IN_A)
        before = self.base.read_text(encoding="utf-8")
        code, out, _ = self.run_gate(TWO_IN_A, "--update")
        self.assertEqual(code, 0)
        self.assertIn("nothing shrank", out)
        self.assertEqual(self.base.read_text(encoding="utf-8"), before)

    def test_update_lowers_and_drops_emptied_keys(self):
        self.seed(TWO_IN_A + ONE_IN_B)
        _, seeded = WT.read_baseline(self.base)
        self.assertEqual(len(seeded), 2, "one accepted line per seeded key")
        code, _, _ = self.run_gate(TWO_IN_A.splitlines(keepends=True)[0], "--update")
        self.assertEqual(code, 0)
        counts, accepted = WT.read_baseline(self.base)
        self.assertEqual(dict(counts), {(A, "TS2339"): 1})
        self.assertEqual(accepted, seeded, "the seed's accepted lines must survive an update")

    def test_update_refuses_to_raise(self):
        self.seed(ONE_IN_B)
        before = self.base.read_text(encoding="utf-8")
        code, _, err = self.run_gate(TWO_IN_A + ONE_IN_B, "--update")
        self.assertEqual(code, 1)
        self.assertIn("accept-increase", err)
        self.assertEqual(self.base.read_text(encoding="utf-8"), before)

    def test_accept_increase_records_the_reason_per_raised_key(self):
        self.seed(ONE_IN_B)
        code, _, _ = self.run_gate(TWO_IN_A + ONE_IN_B, "--accept-increase", "  a  typed   field ")
        self.assertEqual(code, 0)
        text = self.base.read_text(encoding="utf-8")
        self.assertRegex(text, rf"# accepted \d{{4}}-\d{{2}}-\d{{2}} {A} TS2339 2: a typed field\n")
        self.assertNotRegex(text, rf"# accepted \S+ {B} \S+ \d+: a typed field", "an unchanged key gets no line")
        self.assertIn(f"{A}\tTS2339\t2\n", text)

    def test_accept_increase_needs_a_reason(self):
        code, _, _ = self.run_gate(TWO_IN_A, "--accept-increase", "   ")
        self.assertEqual(code, 2)
        self.assertFalse(self.base.exists())

    def test_accept_increase_lowers_what_fell_in_the_same_write(self):
        self.seed(TWO_IN_A)
        one_left = TWO_IN_A.splitlines(keepends=True)[0]
        code, _, _ = self.run_gate(one_left + ONE_IN_B, "--accept-increase", "new row")
        self.assertEqual(code, 0)
        counts, _ = WT.read_baseline(self.base)
        self.assertEqual(dict(counts), {(A, "TS2339"): 1, (B, "TS2322"): 1})
        self.assertEqual(self.run_gate(one_left + ONE_IN_B)[0], 0)

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


class RaisesOnRecord(unittest.TestCase):
    """`--base`: a raised count over the merge-base needs a new `# accepted` line.

    Without it the TSV's counts are taken at face value, and a hand edit from
    43 to 44 lets a new error through with nothing recording why
    (PR #554 review, finding 2).
    """

    BASE = f"# header\n{A}\tTS2339\t2\n"

    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.dir.cleanup)
        self.head = Path(self.dir.name) / "baseline.tsv"
        self.base = Path(self.dir.name) / "base.tsv"
        self.base.write_text(self.BASE, encoding="utf-8")
        self.out = Path(self.dir.name) / "tsc.out"

    def unrecorded(self, head: str, base: str | None = BASE):
        return WT.unrecorded_raises(base, head)

    def test_a_hand_raised_count_is_refused(self):
        problems = self.unrecorded(f"# header\n{A}\tTS2339\t3\n")
        self.assertEqual(len(problems), 1)
        self.assertIn(f"{A} TS2339: 2 -> 3", problems[0])

    def test_a_hand_added_key_is_refused(self):
        self.assertEqual(len(self.unrecorded(f"# header\n{A}\tTS2339\t2\n{B}\tTS2322\t1\n")), 1)

    def test_a_new_accepted_line_for_that_key_and_count_records_it(self):
        head = f"# header\n# accepted 2026-09-26 {A} TS2339 3: typed a field\n{A}\tTS2339\t3\n"
        self.assertEqual(self.unrecorded(head), [])

    def test_an_accepted_line_for_another_count_or_key_does_not(self):
        for line in (f"# accepted 2026-09-26 {A} TS2339 4: x", f"# accepted 2026-09-26 {B} TS2339 3: x",
                     "# accepted 2026-09-26: x"):
            with self.subTest(line=line):
                self.assertEqual(len(self.unrecorded(f"# header\n{line}\n{A}\tTS2339\t3\n")), 1)

    def test_an_accepted_line_the_base_already_had_does_not(self):
        old = f"# accepted 2026-09-01 {A} TS2339 3: earlier"
        base = f"# header\n{old}\n{A}\tTS2339\t2\n"
        self.assertEqual(len(self.unrecorded(f"# header\n{old}\n{A}\tTS2339\t3\n", base)), 1)

    def test_lowering_and_introducing_the_file_need_nothing(self):
        self.assertEqual(self.unrecorded(f"# header\n{A}\tTS2339\t1\n"), [])
        self.assertEqual(self.unrecorded(f"# header\n{A}\tTS2339\t9\n", None), [])

    def test_the_gate_fails_on_an_unrecorded_raise_even_when_counts_match(self):
        self.head.write_text(f"# header\n{A}\tTS2339\t3\n", encoding="utf-8")
        self.out.write_text(
            "".join(f"{A}({n},1): error TS2339: Property 'x' does not exist on type 'Element'.\n" for n in (1, 2, 3)),
            encoding="utf-8")
        args = ["--baseline", str(self.head), "--tsc-output", str(self.out)]
        stderr = io.StringIO()
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(stderr):
            self.assertEqual(WT.main(args), 0, "without --base the counts match")
            self.assertEqual(WT.main(args + ["--base-file", str(self.base)]), 1)
        self.assertIn("with no new '# accepted", stderr.getvalue())

    def test_the_base_is_read_from_git_at_the_merge_base(self):
        text, sha = WT.baseline_at("HEAD")
        head = subprocess.run(["git", "rev-parse", "HEAD"], cwd=ROOT, capture_output=True, text=True,
                              check=True).stdout.strip()
        self.assertEqual(sha, head)
        committed = subprocess.run(["git", "show", "HEAD:tests/web/tsc-baseline.tsv"], cwd=ROOT,
                                   capture_output=True, text=True, check=True).stdout
        self.assertEqual(text, committed)
        with self.assertRaises(LookupError):
            WT.baseline_at("refs/does/not/exist")


class Wired(unittest.TestCase):
    def test_the_fast_lane_checks_raises_against_the_pull_request_base(self):
        # Without --base (or without the history it reads) the provenance
        # check never runs where a pull request is judged.
        lane = (ROOT / ".github/workflows/main-fast-lane.yml").read_text(encoding="utf-8")
        job = lane.split("\n  web_compile:\n", 1)[1].split("\n  apple_compile:\n", 1)[0]
        self.assertIn("fetch-depth: 0", job)
        self.assertIn("PR_BASE_SHA: ${{ github.event.pull_request.base.sha }}", job)
        self.assertIn('scripts/web-types --base "$PR_BASE_SHA"', job)


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
