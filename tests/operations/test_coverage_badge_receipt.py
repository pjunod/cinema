"""Coverage provenance travels with badge bytes; tests push only to local Git."""
import copy
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import textwrap
import unittest


ROOT = Path(__file__).resolve().parents[2]
PUBLISHER = ROOT / "scripts/publish-badge"


class CoverageBadgeReceipt(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="coverage-badge-receipt-")
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.remote = self.directory / "remote.git"
        self.source = self.directory / "source"
        self.real_git = shutil.which("git")
        self.env = {key: value for key, value in os.environ.items()
                    if key != "GITHUB_TOKEN" and not key.startswith("GIT_")}
        self.env.update(GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_SYSTEM=os.devnull,
                        GIT_CONFIG_NOSYSTEM="1", GIT_ALLOW_PROTOCOL="file",
                        GIT_AUTHOR_DATE="2026-10-29T00:00:00+00:00",
                        GIT_COMMITTER_DATE="2026-10-29T00:00:00+00:00")
        template = self.directory / "empty-template"
        template.mkdir()
        self.env["GIT_TEMPLATE_DIR"] = str(template)
        for command in (["init", "--bare", "-q", self.remote],
                        ["init", "-q", self.source],
                        ["-C", self.source, "remote", "add", "origin", self.remote]):
            self.git(*command)
        # Log every push invocation, including an otherwise up-to-date push.
        # The real Git is constrained to local file transport by the environment.
        fake_bin = self.directory / "bin"
        fake_bin.mkdir()
        self.push_log = self.directory / "pushes.jsonl"
        wrapper = fake_bin / "git"
        wrapper.write_text(textwrap.dedent(f"""\
            #!/usr/bin/env python3
            import json, os, sys
            if "push" in sys.argv[1:]:
                with open({str(self.push_log)!r}, "a") as stream:
                    stream.write(json.dumps(sys.argv[1:]) + "\\n")
            os.execv({self.real_git!r}, [{self.real_git!r}, *sys.argv[1:]])
            """))
        wrapper.chmod(0o755)
        self.env["PATH"] = str(fake_bin) + os.pathsep + self.env["PATH"]
        self.args = ["--branch", "badges", "--alias", "codex/badges", "--filename", "coverage",
                     "--label", "unit coverage", "--message", "85.0% · 2026-10-29 UTC",
                     "--fill", "#97ca00", "--json-color", "green",
                     "--label-width", "91", "--message-width", "188"]
        self.receipt = {
            "status": "passed", "source_commit": "a" * 40,
            "source_sha256": "b" * 64, "report_sha256": "c" * 64,
            "line_coverage": {"count": 100, "covered": 85, "percent": 99},
        }

    def git(self, *args):
        return subprocess.check_output([self.real_git, *map(str, args)], env=self.env)

    def remote_git(self, *args):
        return self.git("--git-dir=" + str(self.remote), *args)

    def publish(self, receipt=None):
        command = [str(PUBLISHER), *self.args]
        if receipt is not None:
            command += ["--receipt", str(receipt)]
        return subprocess.run(command, cwd=self.source, env=self.env,
                              capture_output=True, text=True, timeout=15)

    def write_receipt(self, value):
        path = self.directory / "input-receipt.json"
        path.write_text(json.dumps(value))
        return path

    def pushed(self):
        return self.push_log.read_text().splitlines() if self.push_log.exists() else []

    def head(self):
        return self.remote_git("rev-parse", "badges").strip()

    def test_default_two_files_remain_byte_compatible(self):
        result = self.publish()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.remote_git("ls-tree", "--name-only", "badges").decode().splitlines(),
                         ["coverage.json", "coverage.svg"])
        # Exact pre-change publisher outputs for the fixed argv above.
        for name, expected in {
            "coverage.svg": "a9c516c66062ff79fb7ba2322dcef97803765e0d04a9c53d2be15fa323fcd151",
            "coverage.json": "4205d0409d38a041e2019b84e3e70bf4e27f6d458a126443adb20da69fa7de24",
        }.items():
            self.assertEqual(hashlib.sha256(self.remote_git("show", "badges:" + name)).hexdigest(), expected)
        self.assertEqual(self.head(), self.remote_git("rev-parse", "codex/badges").strip())
        self.assertEqual(len(self.pushed()), 1)

    def test_sanitized_receipt_svg_and_json_share_one_orphan_commit(self):
        self.receipt.update(source_commit="A" * 40, source_sha256="B" * 64,
                            measured_at_utc="2026-10-29T14:34:56+02:00", source_epoch=1700000000,
                            output="/private/do-not-publish", environment={"TOKEN": "must-not-leak"},
                            toolchain={"path": "/private/compiler"})
        result = self.publish(self.write_receipt(self.receipt))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.remote_git("ls-tree", "--name-only", "badges").decode().splitlines(),
                         ["coverage-receipt.json", "coverage.json", "coverage.svg"])
        raw = self.remote_git("show", "badges:coverage-receipt.json")
        receipt = json.loads(raw)
        self.assertEqual(receipt, {
            "schema": 1, "status": "passed", "source_commit": "a" * 40,
            "source_sha256": "b" * 64, "report_sha256": "c" * 64,
            "line_coverage": {"count": 100, "covered": 85, "percent": 85.0},
            "measured_at_utc": "2026-10-29T12:34:56+00:00", "source_epoch": 1700000000,
        })
        self.assertNotIn(b"private", raw)
        self.assertNotIn(b"must-not-leak", raw)
        # The new orphan commit has no parent and both aliases identify it.
        self.assertEqual(len(self.remote_git("rev-list", "--parents", "-n", "1", "badges").split()), 1)
        self.assertEqual(self.head(), self.remote_git("rev-parse", "codex/badges").strip())
        self.assertEqual(len(self.pushed()), 1)

    def test_invalid_failed_or_ambiguous_receipts_never_invoke_push(self):
        baseline = self.publish()
        self.assertEqual(baseline.returncode, 0, baseline.stderr)
        before, pushes = self.head(), self.pushed()
        invalid = []
        for key, value in (("status", "failed"), ("source_commit", "g" * 40),
                           ("source_sha256", "b" * 63), ("report_sha256", 10),
                           ("measured_at_utc", "2026-10-29"), ("source_epoch", True)):
            changed = copy.deepcopy(self.receipt); changed[key] = value
            invalid.append(json.dumps(changed))
        for count, covered in ((0, 0), (-1, 0), (100, -1), (100, 101),
                               (True, 1), (100, True), (100, 85.5)):
            changed = copy.deepcopy(self.receipt)
            changed["line_coverage"] = {"count": count, "covered": covered}
            invalid.append(json.dumps(changed))
        missing = copy.deepcopy(self.receipt); del missing["report_sha256"]
        invalid += [json.dumps(missing), "[]", "not JSON", "{\"status\":NaN}",
                    json.dumps(self.receipt).replace('"status": "passed"', '"status":"failed","status":"passed"'),
                    json.dumps(self.receipt) + " " * (64 * 1024)]
        for index, raw in enumerate(invalid):
            with self.subTest(case=index):
                path = self.directory / "invalid.json"; path.write_text(raw)
                result = self.publish(path)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(self.pushed(), pushes)
                self.assertEqual(self.head(), before)

    def test_missing_symlink_fifo_and_directory_receipts_never_invoke_push(self):
        source = self.write_receipt(self.receipt)
        symlink = self.directory / "symlink.json"; symlink.symlink_to(source)
        fifo = self.directory / "fifo.json"; os.mkfifo(fifo)
        for path in (self.directory / "missing.json", symlink, fifo, self.directory):
            with self.subTest(path=path.name):
                result = self.publish(path)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(self.pushed(), [])
        self.assertNotEqual(self.publish("").returncode, 0)
        self.assertEqual(self.pushed(), [])


if __name__ == "__main__":
    unittest.main()
