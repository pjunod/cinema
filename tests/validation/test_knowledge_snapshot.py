"""Synthetic isolated-Git boundaries; run only after final adversarial review.

These fixtures qualify no execution receipt, CI run or real environment.
"""
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import tempfile
import unittest
from unittest.mock import patch

from validation.knowledge import GraphData, KnowledgeError, Receipt, content_digest
from validation.knowledge import model, snapshot as snapshot_module
from validation.knowledge.evidence import assess
from validation.knowledge.index import build, load
from validation.knowledge.snapshot import SnapshotChanged, capture

ENV = {"runtime": "synthetic-fixture", "check_kind": "behavioral"}


class KnowledgeSnapshotTests(unittest.TestCase):
    def setUp(self):
        base = Path(os.environ.get("PLURX_TEST_TMPDIR", Path.cwd() / "target" / "knowledge-tests"))
        base.mkdir(parents=True, exist_ok=True)
        self.directory = tempfile.TemporaryDirectory(prefix="snapshot-", dir=base)
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name) / "repo"
        self.root.mkdir()
        self.run_git("init", "-q")
        self.run_git("config", "user.name", "Synthetic fixture")
        self.run_git("config", "user.email", "fixture@localhost")
        self.write(".gitignore", "/target\n")
        self.write("src/input.rs", "fn route() {}\n")
        self.write("src/check.rs", "fn checked() {}\n")
        self.write("src/dependency.txt", "dependency v1\n")
        self.write("docs/policy.md", "# Synthetic fixture\nNo real qualification.\n")
        self.write("cases/one.json", "{}\n")
        self.write("validation/knowledge/pilot.json", json.dumps({
            "schema_version": 1, "repository": "plurx", "source_commit": "a" * 40,
            "cohorts": {"routing": {"manifest": "tests/playback/routing-decisions.toml", "ids": ["fixture.route"]},
                        "receipt": {"sources": ["src/input.rs"], "policy": "docs/policy.md"}},
            "document_roles": {"docs/policy.md": "reference"}}))
        for name, arrays in (("records", "anchors=[]\nrecords=[]\nedges=[]\nclaims=[]\n"),
                             ("coverage", "scopes=[]\n"), ("conflicts", "conflicts=[]\n")):
            self.write("validation/knowledge/" + name + ".toml",
                       'schema_version=1\nrepository="plurx"\n' + arrays)
        self.write("tests/playback/routing-decisions.toml", '''version=1
[[decisions]]
id="fixture.route"
source="src/input.rs"
source_anchor="fn route("
test="src/check.rs"
test_anchor="fn checked("
''')
        self.write("validation/points.toml", '''version=1
[settings]
profiles=["commit"]
[[checks]]
id="fixture-check"
title="Synthetic definition only"
command="never-executed"
profiles=["commit"]
[[points]]
id="fixture"
title="Synthetic cohort"
contract="No production qualification"
paths=["src/**"]
checks=["fixture-check"]
''')
        self.descriptor = {"id_prefix": "validation:test_main_preflight_adoption.",
                           "inputs": ["src/**", "validation/main-preflight-inputs.json"],
                           "pathsets": ["cases/*.json"], "history": False,
                           "reason": "Synthetic complete witness, no real execution"}
        self.write_descriptor()
        self.commit("fixture: initial synthetic source")
        self.head = self.run_git("rev-parse", "HEAD")
        self.tree = self.run_git("rev-parse", "HEAD^{tree}")

    def run_git(self, *args, root=None):
        return subprocess.check_output(["git", "-C", str(root or self.root), *args],
            env={**os.environ, "GIT_OPTIONAL_LOCKS": "0"}, stderr=subprocess.DEVNULL, text=True).strip()

    def write(self, path, raw):
        target = self.root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(raw)
        return target

    def write_descriptor(self):
        self.write("validation/main-preflight-inputs.json", json.dumps({"version": 1, "families": [self.descriptor]}))

    def commit(self, message):
        self.run_git("add", ".")
        self.run_git("commit", "-q", "-m", message)

    def receipt(self, **changes):
        return Receipt(**{"id": "plurx:receipt:synthetic", "execution_id": "synthetic-only",
            "check_id": "plurx:check:synthetic", "source_revision": self.head,
            "source_tree": self.tree, "environment": ENV, "result": "pass",
            "scope_descriptor": self.descriptor, "descriptor_digest": content_digest(self.descriptor),
            "descriptor_completeness": "canonical", "origin": "reported",
            "artifact_ref": "artifact:sha256:" + "b" * 64, "artifact_digest": "sha256:" + "b" * 64,
            **changes})

    def assessed(self, receipt=None, **options):
        return assess(receipt or self.receipt(), capture(self.root, environment=ENV), self.root,
                      target_environment=ENV, **options)

    def test_staged_unstaged_and_index_identity_are_distinct(self):
        clean = capture(self.root)
        self.write("src/dependency.txt", "unstaged\n")
        dirty = capture(self.root)
        self.run_git("add", "src/dependency.txt")
        staged = capture(self.root)
        self.assertEqual(len({clean.key, dirty.key, staged.key}), 3)
        self.assertFalse(clean.verify(self.root))
        self.assertTrue(staged.verify(self.root))

    def test_two_worktrees_cannot_share_snapshot_or_cache(self):
        other = self.root.parent / "other"
        self.run_git("worktree", "add", "--detach", str(other), self.head)
        first, second = build(self.root), build(other)
        self.assertNotEqual(first.snapshot.worktree_id, second.snapshot.worktree_id)
        self.assertNotEqual(first.snapshot.key, second.snapshot.key)
        self.assertNotEqual(first.database, second.database)
        self.assertEqual(first.graph.to_dict(), second.graph.to_dict())

    def test_allowlisted_untracked_membership_changes_key_but_private_files_are_not_read(self):
        self.write("unrelated.txt", "unrelated owner work\n")
        initial = capture(self.root)
        self.write("src/new.txt", "selected untracked input\n")
        current = capture(self.root)
        self.assertNotEqual(initial.key, current.key)
        self.assertIn("src/new.txt", current.untracked)
        self.assertNotIn("unrelated.txt", {item.path for item in current.inputs})
        self.write("unrelated.txt", "changed unrelated bytes\n")
        self.assertEqual(current.key, capture(self.root).key)
        with self.assertRaises(KnowledgeError):
            capture(self.root, extra_paths=("unrelated.txt",))
        with self.assertRaises(KnowledgeError):
            capture(self.root, extra_paths=(".env",))

    def test_reviewed_broad_pattern_cannot_ingest_private_members(self):
        # The reviewed catalog selects src/**, but every concrete member must
        # independently satisfy the central source policy before stat/open.
        for relative in ("src/.codex/auth.json", "src/.hermes/auth.json", "src/auth.json",
                         "src/__pycache__/cached.pyc", "src/evaluation-private/answer.json"):
            with self.subTest(relative=relative):
                path = self.write(relative, "synthetic credential; never ingest")
                with patch("validation.knowledge.snapshot._read", wraps=snapshot_module._read) as reads:
                    with self.assertRaises(KnowledgeError):
                        capture(self.root)
                self.assertNotIn(relative, [call.args[1] for call in reads.call_args_list])
                # Even direct read callers must pass the policy before any
                # filesystem traversal or stat helper is reached.
                with patch("validation.knowledge.snapshot.checked_path", side_effect=AssertionError("filesystem guard reached")):
                    with self.assertRaises(KnowledgeError):
                        snapshot_module._read(self.root, relative)
                path.unlink()

    def test_delete_rename_mode_and_aba_invalidate(self):
        original = capture(self.root)
        dependency = self.root / "src/dependency.txt"
        raw = dependency.read_bytes()
        dependency.write_bytes(raw + b"changed")
        dependency.write_bytes(raw)
        self.assertFalse(original.verify(self.root))
        before_mode = capture(self.root)
        dependency.chmod(0o755)
        self.assertNotEqual(before_mode.key, capture(self.root).key)
        dependency.rename(self.root / "src/renamed.txt")
        renamed = capture(self.root)
        self.assertNotEqual(before_mode.key, renamed.key)
        (self.root / "src/renamed.txt").unlink()
        self.assertNotEqual(renamed.key, capture(self.root).key)

    def test_symlink_source_and_cache_escape_refuse_without_opening_target(self):
        outside = self.root.parent / "outside-secret"
        outside.write_text("must not be captured")
        (self.root / "src/escape.txt").symlink_to(outside)
        with self.assertRaises(KnowledgeError):
            capture(self.root)
        (self.root / "src/escape.txt").unlink()
        (self.root / "target").symlink_to(self.root.parent)
        with self.assertRaises(KnowledgeError):
            build(self.root)

    def test_cache_is_atomic_sqlite_and_graph_roundtrips_from_frozen_corpus(self):
        indexed = build(self.root)
        with sqlite3.connect(indexed.database) as connection:
            tables = {row[0] for row in connection.execute("SELECT name FROM sqlite_master WHERE type='table'")}
            self.assertTrue({"meta", "nodes", "edges", "claims", "source_anchors", "receipts", "coverage", "conflicts"} <= tables)
            payload = json.loads(connection.execute("SELECT value FROM meta WHERE name=?", ("graph",)).fetchone()[0])
        self.assertEqual(GraphData.from_dict(payload).to_dict(), indexed.graph.to_dict())
        corpus = indexed.database.parent / "corpus"
        self.assertEqual((corpus / "src/input.rs").read_bytes(), b"fn route() {}\n")
        self.assertEqual(model.load_graph(corpus, revision=indexed.snapshot.head).to_dict(), indexed.graph.to_dict())
        self.assertEqual(self.run_git("status", "--porcelain"), "")

    def test_mid_index_mutation_retries_once_and_publishes_no_mixed_graph(self):
        actual = model.load_graph
        frozen_calls = []
        def change_after_frozen(root, **kwargs):
            graph = actual(root, **kwargs)
            if Path(root) != self.root:
                frozen_calls.append(root)
                self.write("src/dependency.txt", "mutation " + str(len(frozen_calls)))
            return graph
        with patch("validation.knowledge.index.model.load_graph", side_effect=change_after_frozen):
            with self.assertRaises(SnapshotChanged):
                build(self.root)
        self.assertEqual(len(frozen_calls), 2)
        self.assertFalse(list((self.root / "target/knowledge").rglob("graph.sqlite")))
        self.assertFalse(list((self.root / "target/knowledge").rglob(".build-*")))

    def test_load_revalidates_and_prunes_old_owned_bundles(self):
        snapshots = []
        for n in range(4):
            self.write("src/dependency.txt", str(n))
            snapshots.append(load(self.root))
        self.assertEqual(len({item.snapshot.key for item in snapshots}), 4)
        self.assertEqual(len(list(snapshots[-1].database.parent.parent.glob("bundle-*"))), 2)

    def test_named_check_is_not_execution_and_raw_origin_cannot_authenticate(self):
        check = next(node for node in model.load_graph(self.root).nodes if node.kind == "check")
        dimensions = self.assessed(check)
        self.assertEqual(dimensions["execution"], "not-run")
        self.assertEqual(dimensions["origin"], "unavailable")
        forged = assess({"origin": "authenticated", "result": "pass"}, capture(self.root), self.root)
        self.assertFalse(forged["authenticated_credit"])
        with self.assertRaises(KnowledgeError):
            self.receipt(origin="authenticated")

    def test_reported_pass_fail_skip_and_not_run_preserve_execution(self):
        for execution in ("pass", "fail", "skip", "not-run"):
            with self.subTest(execution=execution):
                result = self.assessed(self.receipt(result=execution))
                self.assertEqual(result["execution"], execution)
                self.assertEqual(result["origin"], "reported")
                self.assertEqual(result["source_compatibility"], "exact-tree")
                self.assertEqual(result["overall_applicability"], "unknown")
                self.assertFalse(result["authenticated_credit"])

    def test_global_dirty_target_cannot_claim_exact_tree(self):
        self.write("unrelated.txt", "global dirtiness\n")
        result = self.assessed(descriptor_complete=True)
        self.assertNotEqual(result["source_compatibility"], "exact-tree")
        self.assertEqual(result["source_compatibility"], "compatible-within-reviewed-scope")

    def test_complete_unchanged_closure_is_compatible_but_unauthenticated(self):
        self.write("unrelated.txt", "new committed unrelated tree\n")
        self.commit("fixture: unrelated source outside reviewed closure")
        result = self.assessed(descriptor_complete=True)
        self.assertEqual(result["source_compatibility"], "compatible-within-reviewed-scope")
        self.assertEqual(result["origin"], "reported")
        self.assertFalse(result["authenticated_credit"])

    def test_changed_dependency_pathset_mode_and_descriptor_are_stale(self):
        for path, raw in (("src/dependency.txt", "new dependency\n"), ("cases/two.json", "{}\n")):
            self.write(path, raw)
            self.assertEqual(self.assessed(descriptor_complete=True)["source_compatibility"], "stale")
            self.run_git("reset", "--hard", self.head)
            if (self.root / path).exists() and path == "cases/two.json":
                (self.root / path).unlink()
        (self.root / "src/dependency.txt").chmod(0o755)
        self.assertEqual(self.assessed(descriptor_complete=True)["source_compatibility"], "stale")
        self.run_git("reset", "--hard", self.head)
        old_receipt = self.receipt()
        self.descriptor = {**self.descriptor, "inputs": ["src/input.rs"]}
        self.write_descriptor()
        result = self.assessed(old_receipt, descriptor_complete=True)
        self.assertEqual(result["source_compatibility"], "stale")

    def test_missing_history_environment_and_incomplete_closure_are_unknown(self):
        self.assertEqual(self.assessed(self.receipt(source_revision="f" * 40), descriptor_complete=True)["source_compatibility"], "unknown")
        current = capture(self.root, environment=ENV)
        mismatch = assess(self.receipt(), current, self.root, target_environment={"runtime": "different"})
        self.assertEqual(mismatch["source_compatibility"], "unknown")
        self.write("unrelated.txt", "dirty\n")
        incomplete = self.assessed(self.receipt(descriptor_completeness="impact-only"), descriptor_complete=True)
        self.assertEqual(incomplete["source_compatibility"], "unknown")
        self.assertEqual(self.assessed(descriptor_complete=False)["source_compatibility"], "unknown")

    def test_canonical_history_cannot_be_narrowed_by_retrieval_scope(self):
        self.descriptor = {**self.descriptor, "history": True}
        self.write_descriptor()
        self.commit("fixture: canonical history witness")
        self.head = self.run_git("rev-parse", "HEAD")
        self.tree = self.run_git("rev-parse", "HEAD^{tree}")
        receipt = self.receipt()
        self.write("unrelated.txt", "new history\n")
        self.commit("fixture: new history even though proof source is unchanged")
        result = self.assessed(receipt, descriptor_complete=True, dependency_paths=["src/input.rs"])
        self.assertEqual(result["source_compatibility"], "stale")

    def test_missing_and_malformed_discovery_metadata_refuse_explicitly(self):
        path = self.root / "validation/knowledge/records.toml"
        original = path.read_bytes()
        path.unlink()
        with self.assertRaises(KnowledgeError):
            capture(self.root)
        path.write_text("not a TOML document = [")
        with self.assertRaises(KnowledgeError):
            capture(self.root)
        path.write_bytes(original)
        self.assertTrue(capture(self.root).verify(self.root))

    def test_private_environment_metadata_and_materialization_escape_refuse(self):
        for environment in ({"token": "private"}, {"runtime": "/owner/private-path"}):
            with self.assertRaises(KnowledgeError):
                capture(self.root, environment=environment)
        snapshot = capture(self.root)
        text = json.dumps(snapshot.metadata())
        self.assertNotIn(str(self.root), text)
        self.assertNotIn("fn route", text)
        destination = self.root.parent / "materialized"
        destination.mkdir()
        (destination / "src").symlink_to(self.root.parent)
        with self.assertRaises(KnowledgeError):
            snapshot.materialize(destination)


if __name__ == "__main__":
    unittest.main()
