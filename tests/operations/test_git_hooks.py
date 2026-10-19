"""Git hook children must discover their own disposable repositories."""
from __future__ import annotations

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import textwrap
import unittest


ROOT = Path(__file__).resolve().parents[2]


class GitHookEnvironmentCase(unittest.TestCase):
    def test_pre_push_scrubs_repository_environment_before_paperwork_fixtures(self):
        # Never let an enclosing hook's own environment touch this test's setup.
        clean = os.environ.copy()
        names = subprocess.run(
            ["git", "rev-parse", "--local-env-vars"], cwd=ROOT,
            capture_output=True, text=True, check=True).stdout.splitlines()
        for name in names:
            clean.pop(name, None)
        clean.update(GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1")

        with tempfile.TemporaryDirectory(prefix="plurx-hook-environment-") as directory:
            root = Path(directory)
            parent = root / "parent"
            linked = root / "linked"
            remote = root / "remote.git"
            fixtures = root / "fixtures"

            def git(*arguments, cwd=parent):
                return subprocess.run(
                    ["git", *arguments], cwd=cwd, env=clean, check=True,
                    capture_output=True, text=True).stdout.strip()

            parent.mkdir()
            git("init", "-q")
            git("config", "user.name", "Hook Fixture")
            git("config", "user.email", "hook-fixture@example.invalid")
            git("config", "commit.gpgsign", "false")
            scripts = parent / "scripts"
            scripts.mkdir()
            # Exercise the tracked hook itself, with a small stand-in for the
            # paperwork program that deliberately performs the dangerous Git
            # fixture operations. All metadata involved belongs to this test.
            probe = textwrap.dedent('''\
                import os
                from pathlib import Path
                import subprocess
                import sys

                assert sys.argv[1:] == ["--hook"]
                fixture_root = Path(os.environ["PLURX_HOOK_FIXTURES"])
                fixture_root.mkdir()
                inherited = subprocess.check_output(
                    ["git", "rev-parse", "--local-env-vars"], text=True).splitlines()
                assert not any(name in os.environ for name in inherited), {
                    name: os.environ[name] for name in inherited if name in os.environ}
                bare = fixture_root / "nested.git"
                subprocess.run(["git", "init", "-q", "--bare", str(bare)], check=True)
                normal = fixture_root / "nested"
                subprocess.run(["git", "init", "-q", str(normal)], check=True)
                def git(*args):
                    return subprocess.run(["git", *args], cwd=normal, check=True,
                        capture_output=True, text=True).stdout.strip()
                git("config", "user.name", "Nested Fixture")
                git("config", "user.email", "nested@example.invalid")
                git("config", "commit.gpgsign", "false")
                (normal / "sentinel").write_text("nested fixture\\n")
                git("add", "sentinel")
                git("commit", "-qm", "nested fixture")
                assert git("rev-parse", "--show-toplevel") == str(normal.resolve())
                assert git("ls-files") == "sentinel"
                assert git("config", "--get", "core.bare") == "false"
                (fixture_root / "complete").write_text(git("rev-parse", "HEAD") + "\\n")
                ''')
            (scripts / "paperwork").write_text(probe)
            git("add", "scripts")
            git("commit", "-qm", "fixture parent")
            git("init", "-q", "--bare", str(remote))
            git("worktree", "add", "-q", "-b", "linked-fixture", str(linked))
            common = Path(git("rev-parse", "--absolute-git-dir"))
            linked_git = Path(git("rev-parse", "--absolute-git-dir", cwd=linked))
            hook = common / "hooks/pre-push"
            shutil.copy2(ROOT / "scripts/pre-push", hook)
            hook.chmod(0o755)

            def snapshot():
                return {
                    "config": (common / "config").read_bytes(),
                    "main_index": (common / "index").read_bytes(),
                    "linked_index": (linked_git / "index").read_bytes(),
                    "main_head": (common / "HEAD").read_bytes(),
                    "linked_head": (linked_git / "HEAD").read_bytes(),
                    "refs": git("show-ref"),
                    "main_tree": git("status", "--porcelain=v1"),
                    "linked_tree": git("status", "--porcelain=v1", cwd=linked),
                }

            before = snapshot()
            contaminated = {
                **clean,
                "GIT_DIR": str(linked_git),
                "GIT_COMMON_DIR": str(common),
                "GIT_WORK_TREE": str(linked),
                "GIT_INDEX_FILE": str(linked_git / "index"),
                "GIT_OBJECT_DIRECTORY": str(common / "objects"),
                "GIT_CONFIG_COUNT": "1",
                "GIT_CONFIG_KEY_0": "core.bare",
                "GIT_CONFIG_VALUE_0": "false",
                "PLURX_HOOK_FIXTURES": str(fixtures),
            }
            # A real local push invokes the installed actual hook in the same
            # manner as a linked-worktree push; it publishes only to our temp repo.
            pushed = subprocess.run(
                ["git", "push", str(remote), "HEAD:refs/heads/qualified"],
                cwd=linked, env=contaminated, capture_output=True, text=True,
                check=False)
            self.assertEqual(pushed.returncode, 0, pushed.stdout + pushed.stderr)
            self.assertTrue((fixtures / "complete").is_file())
            self.assertTrue((fixtures / "nested.git/HEAD").is_file())
            self.assertTrue((fixtures / "nested/.git/HEAD").is_file())
            self.assertEqual(snapshot(), before)
            self.assertEqual(git("config", "--get", "core.bare"), "false")
            self.assertEqual(git("rev-parse", "HEAD", cwd=linked),
                             git("rev-parse", "refs/heads/qualified", cwd=remote))
