"""Pre-checkout workspace classification, without Docker or runner access."""
import ast
from pathlib import Path
import re
import textwrap
import unittest

ROOT = Path(__file__).resolve().parents[2]
WORKSPACE = "/workspace/noirr/plurx"
TOKEN = "ec88db3044acd17c4a43814d7e48fff6"


def row(mid, root, path, options="rw", device="259:4"):
    return f"{mid} 1 {device} {root} {path} {options},relatime - ext4 /dev/test rw"


def mounted():
    return "\n".join((
        row(101, f"/var/lib/docker/volumes/{TOKEN}/_data", WORKSPACE),
        row(102, f"/var/lib/docker/volumes/{TOKEN}-env/_data", "/run/act"),
    ))


class RunnerWorkspaceCase(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.workflow = (ROOT / ".github/workflows/ci.yml").read_text()
        cls.blocks = re.findall(r"          import os\n(.*?)          PYTHON\n", cls.workflow, re.S)
        assert len(cls.blocks) == 4
        code = textwrap.dedent("          import os\n" + cls.blocks[0])
        tree = ast.parse(code)
        tree.body = [node for node in tree.body if isinstance(node, (ast.Import, ast.FunctionDef))]
        namespace = {}
        exec(compile(tree, "<workflow workspace classifier>", "exec"), namespace)
        cls.classify = staticmethod(namespace["ephemeral_workspace"])

    def classify_mounts(self, info, env=None):
        return self.classify(WORKSPACE, "/run/act", info,
                             {"ACT": "true", "FORGEJO_ACTIONS": "true"} if env is None else env)

    def test_exact_paired_forgejo_workspace_uses_provisioner_cleanup(self):
        self.assertTrue(self.classify_mounts(mounted()))
        self.assertEqual(len(set(self.blocks)), 1)
        self.assertEqual(self.workflow.count("PLURX_WORKSPACE_MODE=forgejo-ephemeral"), 4)
        self.assertEqual(self.workflow.count("echo 'HOST_WORKSPACE_OWNER='"), 4)

    def test_host_bind_and_persistent_named_volume_are_not_ephemeral(self):
        for root in ("/opt/runner/_work/noirr/plurx", "/var/lib/docker/volumes/plurx-persistent/_data", "/"):
            with self.subTest(root=root):
                info = mounted().replace(f"/var/lib/docker/volumes/{TOKEN}/_data", root)
                self.assertFalse(self.classify_mounts(info))

    def test_missing_foreign_duplicate_and_readonly_mounts_refuse(self):
        for info in (mounted().splitlines()[0], mounted().replace(TOKEN + "-env", "f" * 32 + "-env"),
                     mounted() + "\n" + mounted().splitlines()[0], mounted().replace("rw,relatime", "ro,relatime"),
                     mounted().replace("102 1 259:4", "102 1 259:5"),
                     mounted().replace("/run/act", "/run/foreign")):
            with self.subTest(info=info):
                self.assertFalse(self.classify_mounts(info))

    def test_unknown_provisioner_and_malformed_mountinfo_refuse(self):
        for env in ({}, {"ACT": "true"}, {"ACT": "false", "FORGEJO_ACTIONS": "true"}):
            self.assertFalse(self.classify_mounts(mounted(), env))
        for info in ("", "invalid", mounted().replace("- ext4 /dev/test rw", "-"),
                     mounted().replace("101 1 ", "x 1 "), mounted().replace("102 1 ", "101 1 ")):
            self.assertFalse(self.classify_mounts(info))

    def test_persistent_owner_guard_and_restore_remain_fail_closed(self):
        self.assertEqual(self.workflow.count("No non-root mounted workspace owner"), 4)
        self.assertEqual(self.workflow.count("echo 'PLURX_WORKSPACE_MODE=persistent-bind'"), 4)
        self.assertEqual(self.workflow.count('if [ "${PLURX_WORKSPACE_MODE:-}" = persistent-bind ] && [ -n "${HOST_WORKSPACE_OWNER:-}" ]; then'), 4)
        self.assertEqual(self.workflow.count('chown -R "$HOST_WORKSPACE_OWNER" "$GITHUB_WORKSPACE"'), 4)
        self.assertNotIn("chown -R 1001", self.workflow)
        self.assertNotIn("chown -R 1000", self.workflow)
