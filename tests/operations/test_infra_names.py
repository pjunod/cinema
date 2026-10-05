"""The tracked tree names no real host, LAN address, home or SSH user.

The repository is pushed to a public mirror, and PR #332 rewrote the real lab
out of it once — with no guard, so the names came back through new docs,
evidence and tests within days. `scripts/scrub-infra-names` owns the
mapping and its context rules, and `scripts/scrub-infra-names.allow.toml` the
exceptions; this test runs its check over every tracked file, so a new real
name fails `make operations-check` with file:line instead of waiting for the
next sweep. Fix a failure with `scripts/scrub-infra-names --write` (it
rewrites in place), or, if a literal truly must stay, add the file to the
allowlist with the reason.

The real names below are spelled in pieces so this file is not itself a hit.
"""

from __future__ import annotations

import contextlib
import hashlib
import importlib.machinery
import importlib.util
from pathlib import Path
import io
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "scrub-infra-names"

loader = importlib.machinery.SourceFileLoader("scrub_infra_names", str(SCRIPT))
spec = importlib.util.spec_from_loader(loader.name, loader)
scrub = importlib.util.module_from_spec(spec)
sys.modules[loader.name] = scrub  # dataclasses resolve annotations through it
loader.exec_module(scrub)

NY = "ny" + "nuc"
LAN4 = "192.168" + ".4."
LAN5 = "192.168" + ".5."
HOME = "/Users/" + "pjunod"
USER = "pjunod" + "@"


class TrackedTreeTests(unittest.TestCase):
    def test_tracked_tree_names_no_real_infrastructure(self) -> None:
        violations = scrub.scan(ROOT)
        self.assertEqual(
            [], [str(v) for v in violations],
            "real infrastructure names in the tracked tree; run "
            "scripts/scrub-infra-names --write (or allowlist a literal that must stay)")

    def test_every_allowlist_entry_still_covers_a_real_hit(self) -> None:
        tracked = scrub.tracked_files(ROOT)
        for glob, (rules, reason) in scrub.load_allowlist().items():
            with self.subTest(glob=glob):
                matched = [p for p in tracked if scrub._matches(p, [glob])]
                self.assertTrue(matched, f"stale allowlist entry {glob}")
                self.assertTrue(reason.strip())
                hits = [rule for p in matched
                        for _, rule, _ in scrub.findings(scrub.read_text(ROOT / p) or "")
                        if scrub.ALL_RULES in rules or rule in rules]
                self.assertTrue(hits, f"allowlist entry {glob} no longer exempts anything; remove it")

    def test_release_inputs_carry_no_real_names(self) -> None:
        # A hit here cannot be fixed by --write: it needs a client build.
        for v in scrub.scan(ROOT):
            self.assertFalse(scrub._matches(v.path, scrub.RELEASE_INPUTS), str(v))


class MappingTests(unittest.TestCase):
    def rewrite(self, text: str) -> str:
        return scrub.rewrite(text)[0]

    def test_hosts_and_runner_names_map_to_neutral_names(self) -> None:
        cases = {
            f"on {NY} and nuc" "3": "on media1 and lab3",
            "gha-mb" "a-apple-01": "gha-maca-apple-01",
            "gha-" "nuc" "4-general-01": "gha-lab4-general-01",
            "gha-" "rogg" "16-general-05": "gha-lab5-general-05",
            "gha-mb" "p-linux-arm-01": "gha-macb-linux-arm-01",
            "gha-m" "6-general-03": "gha-lab6-general-03",
            f"{NY}/m" "6/nuc" "4": "media1/lab6/lab4",
            "m" "6_last_index_stamp": "lab6_last_index_stamp",
            "--hardware-label m" "6-pro": "--hardware-label lab6-pro",
            "Ny" "nuc runner": "Media1 runner",
            "NY" "NUC-RUNNER.md": "MEDIA1-RUNNER.md",
            "QN" "AP NFS": "NAS NFS",
            "the configured `bil" "ly` jump host": "the configured `jump1` jump host",
            "ssh -J bil" "ly lab3": "ssh -J jump1 lab3",
            # A slash alone is a path separator, not encoded data.
            "logs/mb" "a/xcodebuild.log": "logs/maca/xcodebuild.log",
            "ran on m" "6.": "ran on lab6.",
            "mb" "p.local": "macb.local",
        }
        for old, new in cases.items():
            with self.subTest(old=old):
                self.assertEqual(new, self.rewrite(old))

    def test_milestone_and_substrings_are_not_hosts(self) -> None:
        for text in ("fmp4", "m64", "combat", "mbar", "agent/m" "6-handoff",
                     "agent/m" "6-android-prepared-replacement", "codex/ffmpeg-scratch-m" "6",
                     "status-m" "6-close", "playback-caps-v2-m1..m" "6", "M6 handoff",
                     "MBA laptop", "nuc5", "nuc" "34", "xm" "6", "m" "6x",
                     "Bil" "ly Elliot", "BIL" "LY", "hillbil" "ly", "bil" "lyjoe"):
            with self.subTest(text=text):
                self.assertEqual(text, self.rewrite(text))

    def test_short_names_inside_encoded_data_are_not_hosts(self) -> None:
        base64 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAAB" "/mb" "a/" "CAYAAAAfFcSJAAAADUlEQVR42mNk"
        for text in (
            'd="M12 2l1.5 3m' '6.5 0"',          # SVG relative moveto, decimal
            'd="M0 0h2m' '6.25-1"',
            "data:image/png;base64," + base64,  # slash-bounded inside base64
            "AAAA+m" "6+BBBB",                   # plus-bounded
            "QUJD+mb" "p=",
        ):
            with self.subTest(text=text):
                self.assertEqual(text, self.rewrite(text))
                self.assertEqual([], scrub.findings(text))

    def test_addresses_homes_and_ssh_users(self) -> None:
        cases = {
            f"http://{LAN4}7:3000/noirr/plurx/pulls/1": "http://forge.lan:3000/noirr/plurx/pulls/1",
            f"{LAN4}7:3000/noirr/plurxd:main": "forge.lan:3000/noirr/plurxd:main",
            f"http://{LAN4}7:32400": "http://10.42.4.7:32400",
            f"{LAN4}7:30000": "10.42.4.7:30000",
            f"{LAN5}236 and {LAN4}0/24": "10.42.5.236 and 10.42.4.0/24",
            f"ssh {USER}{LAN5}115": "ssh operator@10.42.5.115",
            f"{HOME}/code/plurx": "~/code/plurx",
            "/home/" "pjunod/work": "~/work",
        }
        for old, new in cases.items():
            with self.subTest(old=old):
                self.assertEqual(new, self.rewrite(old))

    def test_other_private_ranges_and_lookalikes_stay(self) -> None:
        for text in ("http://192.168.1.10:32400", "192.168.45.3", "10192.168.4.1x",
                     "pjunod/plurx", '"login": "pjunod"', "/Users/pjunodx"):
            with self.subTest(text=text):
                self.assertEqual(text, self.rewrite(text))

    def test_rewrite_is_idempotent(self) -> None:
        once = self.rewrite(f"{NY} {LAN4}7:3000 {LAN5}9 gha-m" "6-general-01 {HOME} {USER}x")
        self.assertEqual(once, self.rewrite(once))
        self.assertEqual([], scrub.findings(once))

    def test_retired_product_name_is_reported_never_rewritten(self) -> None:
        text = "the " + "cine" "marr" + " app"
        self.assertEqual(text, self.rewrite(text))
        self.assertEqual(["retired-name"], [rule for _, rule, _ in scrub.findings(text)])


class CheckModeTests(unittest.TestCase):
    def repo(self, tmp: str) -> Path:
        root = Path(tmp)
        subprocess.run(["git", "init", "-q", str(root)], check=True)
        (root / "notes").mkdir()
        (root / "notes" / "a.md").write_text(f"ok\nran on {NY}\n")
        (root / "notes" / "kept.json").write_text(f"{LAN4}9\n")
        (root / "README.md").write_text("cine" "marr" " mark\n")
        (root / "notes" / f"{NY.upper()}.md").write_text("fine\n")
        (root / "allow.toml").write_text(
            '[[allow]]\npath = "notes/kept.json"\nreason = "pinned"\n\n'
            '[[allow]]\npath = "README.md"\nrules = ["retired-name"]\nreason = "mark"\n')
        subprocess.run(["git", "-C", str(root), "add", "-A"], check=True)
        return root

    def test_check_lists_file_line_and_respects_the_allowlist_file(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = self.repo(tmp)
            allow = scrub.load_allowlist(root / "allow.toml")
            found = sorted(str(v) for v in scrub.scan(root, allow))
            self.assertEqual(
                [f"notes/{NY.upper()}.md:0: host (path): {NY.upper()}",
                 f"notes/a.md:2: host: {NY}"], found)
            args = ["--check", "--root", str(root), "--allowlist", str(root / "allow.toml")]
            with contextlib.redirect_stdout(io.StringIO()) as out, \
                    contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(1, scrub.main(args))
            self.assertIn(f"notes/a.md:2: host: {NY}", out.getvalue())

    def test_write_rewrites_renames_and_leaves_the_allowlisted_file(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = self.repo(tmp)
            args = ["--write", "--root", str(root), "--allowlist", str(root / "allow.toml")]
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(0, scrub.main(args))
            self.assertEqual("ok\nran on media1\n", (root / "notes" / "a.md").read_text())
            self.assertTrue((root / "notes" / "MEDIA1.md").exists())
            self.assertEqual(f"{LAN4}9\n", (root / "notes" / "kept.json").read_text())

    def test_write_refuses_a_file_whose_digest_is_pinned(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = self.repo(tmp)
            leaf = root / "notes" / "a.md"
            digest = hashlib.sha256(leaf.read_bytes()).hexdigest()
            (root / "notes" / "receipt.json").write_text(f'{{"sha256": "{digest}"}}\n')
            subprocess.run(["git", "-C", str(root), "add", "-A"], check=True)
            args = ["--write", "--root", str(root), "--allowlist", str(root / "allow.toml")]
            with contextlib.redirect_stdout(io.StringIO()) as out:
                self.assertEqual(1, scrub.main(args))
            self.assertIn("PINNED", out.getvalue())
            self.assertIn(NY, leaf.read_text())

    def test_mode_is_required_and_bad_allowlists_are_refused(self) -> None:
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
            scrub.main([])
        with tempfile.TemporaryDirectory() as tmp:
            bad = Path(tmp) / "allow.toml"
            for body in ('[[allow]]\npath = "x"\n',
                         '[[allow]]\npath = "x"\nreason = "r"\nrules = ["nope"]\n',
                         '[[allow]]\npath = "x"\nreason = "r"\nwhy = "?"\n'):
                bad.write_text(body)
                with self.subTest(body=body), self.assertRaises(ValueError):
                    scrub.load_allowlist(bad)


if __name__ == "__main__":
    unittest.main()
