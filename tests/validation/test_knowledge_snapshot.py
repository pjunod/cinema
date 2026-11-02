"""Synthetic isolated-Git boundaries; run only after final adversarial review.

These fixtures qualify no execution receipt, CI run or real environment.
"""
from dataclasses import replace
from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import tempfile
import unittest
from unittest.mock import patch

from validation.knowledge import GraphData, KnowledgeError, Receipt, content_digest, digest_bytes
from validation.knowledge import index as index_module, model, snapshot as snapshot_module
from validation.knowledge.evidence import assess
from validation.knowledge.index import build, load
from validation.knowledge.snapshot import SnapshotChanged, capture

# Paths below belong to disposable repositories, not this documentation tree.
FIXTURE_DOC_ROOT = "docs"

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
        policy = self.write(f"{FIXTURE_DOC_ROOT}/policy.md", "# Synthetic fixture\nNo real qualification.\n")
        self.write("cases/one.json", "{}\n")
        self.write("validation/knowledge/pilot.json", json.dumps({
            "schema_version": 1, "repository": "plurx", "source_commit": "a" * 40,
            "cohorts": {"routing": {"manifest": "tests/playback/routing-decisions.toml", "ids": ["fixture.route"]},
                        "receipt": {"sources": ["src/input.rs"], "policy": f"{FIXTURE_DOC_ROOT}/policy.md"}},
            "document_roles": {f"{FIXTURE_DOC_ROOT}/policy.md": "reference"}}))
        for name, arrays in (("records", "anchors=[]\nrecords=[]\nedges=[]\nclaims=[]\n"),
                             ("coverage", "scopes=[]\n"), ("conflicts", "conflicts=[]\n")):
            self.write("validation/knowledge/" + name + ".toml",
                       'schema_version=1\nrepository="plurx"\n' + arrays)
        self.write("validation/knowledge/records.toml", f'''schema_version=1
repository="plurx"
anchors=[]
edges=[]
claims=[]
[[records]]
id="plurx:check:fixture-reviewed"
kind="check"
title="Explicit reviewed synthetic source scope"
scope=["src/**", "cases/*.json"]
anchors=[]
[records.attributes]
check_kind="static"
governed_ids=[]
reference="{FIXTURE_DOC_ROOT}/policy.md"
[records.provenance]
method="reviewed"
author="Synthetic fixture author"
reviewer="Synthetic fixture reviewer"
origin="{FIXTURE_DOC_ROOT}/policy.md"
source_digest="{digest_bytes(policy.read_bytes())}"
revision="aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
''')
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
        # The reviewed authored record selects src/**; every concrete member must
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

    def test_impact_and_definition_scopes_do_not_capture_unrelated_bodies_but_receipt_proof_does(self):
        catalog = self.root / "validation/points.toml"
        catalog.write_text(catalog.read_text().replace('paths=["src/**"]', 'paths=["src/**", "unrelated/**"]'))
        self.descriptor = {**self.descriptor,
                           "inputs": [*self.descriptor["inputs"], "proof/**"],
                           "pathsets": [*self.descriptor["pathsets"], "proof-members/*.json"]}
        self.write_descriptor()
        self.write("unrelated/body.py", "unrelated impact body; never capture")
        self.write("proof/dependency.py", "canonical receipt proof")
        self.write("proof-members/new.json", "{}")
        graph = model.load_graph(self.root)
        definitions = snapshot_module.selection(graph)
        selected, _ = snapshot_module._members(self.root, definitions)
        self.assertNotIn("unrelated/body.py", selected)
        self.assertNotIn("proof/dependency.py", selected)
        self.assertNotIn("proof-members/new.json", selected)
        # A narrower receipt retrieval claim cannot erase the canonical linked
        # check's complete input/path-set patterns.
        check = next(node for node in graph.nodes if node.attributes.get("check_kind") == "canonical-preflight")
        narrow = {"inputs": ["src/input.rs"]}
        receipt = self.receipt(check_id=check.id, scope_descriptor=narrow,
                               descriptor_digest=content_digest(narrow))
        actual = replace(graph, receipts=(receipt,))
        actual_selection = snapshot_module.selection(actual)
        selected, _ = snapshot_module._members(self.root, actual_selection)
        self.assertNotIn("unrelated/body.py", selected)
        self.assertIn("proof/dependency.py", selected)
        self.assertIn("proof-members/new.json", selected)

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

    def test_discovery_read_race_retries_even_when_git_identity_is_restored(self):
        actual = model.load_graph
        attempts = []
        def observed_race(root, **kwargs):
            attempts.append(root)
            if len(attempts) == 1:
                raise model.SourceReadChanged("observed write-and-restore during discovery")
            return actual(root, **kwargs)
        with patch("validation.knowledge.snapshot.model.load_graph", side_effect=observed_race):
            current = capture(self.root)
        self.assertEqual(2, len(attempts))
        self.assertEqual(self.head, current.head)
        with patch("validation.knowledge.snapshot.model.load_graph",
                   side_effect=model.SourceReadChanged("continuous discovery race")) as racing:
            with self.assertRaises(SnapshotChanged):
                capture(self.root)
        self.assertEqual(2, racing.call_count)

    def test_load_revalidates_and_prunes_old_owned_bundles(self):
        snapshots = []
        for n in range(4):
            self.write("src/dependency.txt", str(n))
            snapshots.append(load(self.root))
        self.assertEqual(len({item.snapshot.key for item in snapshots}), 4)
        self.assertEqual(len(list(snapshots[-1].database.parent.parent.glob("bundle-*"))), 2)

    def test_warm_hit_reuses_authenticated_graph_without_discovery_or_reparse(self):
        cold = build(self.root)
        with patch("validation.knowledge.index.capture", side_effect=AssertionError("unexpected cold capture")), \
             patch("validation.knowledge.index.model.load_graph", side_effect=AssertionError("unexpected M1 reparse")):
            warm = load(self.root)
        self.assertEqual(warm.database, cold.database)
        self.assertEqual(warm.graph.to_dict(), cold.graph.to_dict())
        self.assertEqual(warm.snapshot.key, cold.snapshot.key)
        self.assertTrue(warm.snapshot.verify(self.root))
        self.assertNotIn("cache_signature", warm.snapshot.metadata())
        cache = warm.database.parent.parent.parent
        self.assertEqual((cache.stat().st_mode & 0o777), 0o700)
        self.assertEqual(((cache / ".cache-key").stat().st_mode & 0o777), 0o600)

    def test_warm_cache_invalidates_source_index_modes_dirty_membership_and_branch(self):
        current = build(self.root)
        def changed():
            nonlocal current
            previous = current
            current = load(self.root)
            self.assertNotEqual(current.snapshot.key, previous.snapshot.key)
            self.assertNotEqual(current.database, previous.database)
            self.assertTrue(current.snapshot.verify(self.root))
        self.write("src/dependency.txt", "different selected bytes\n")
        changed()
        self.run_git("add", "src/dependency.txt")
        changed()
        (self.root / "src/dependency.txt").chmod(0o755)
        changed()
        self.write("src/new-input.txt", "new selected member\n")
        changed()
        self.run_git("switch", "-c", "fixture-warm-branch")
        changed()
        self.assertTrue(current.snapshot.identity["branch"].endswith("fixture-warm-branch"))
        self.run_git("reset", "--hard", self.head)
        (self.root / "src/new-input.txt").unlink()
        current = load(self.root)
        self.assertFalse(current.snapshot.repository_dirty)
        self.write("unrelated-owner-note.txt", "outside selected graph but globally dirty\n")
        changed()
        self.assertTrue(current.snapshot.repository_dirty)

    def test_warm_cache_implementation_and_environment_identity_cannot_cross_views(self):
        first = build(self.root)
        original_versions = snapshot_module.versions()
        changed_versions = {**original_versions, "synthetic-implementation-boundary": "sha256:" + "e" * 64}
        with patch("validation.knowledge.snapshot.versions", return_value=changed_versions):
            changed = load(self.root)
            self.assertNotEqual(changed.snapshot.key, first.snapshot.key)
            self.assertNotEqual(changed.database, first.database)
        # The prior bundle remains valid for the restored exact implementation.
        restored = load(self.root)
        self.assertEqual(restored.database, first.database)
        self.assertEqual(restored.snapshot.key, first.snapshot.key)
        recipe = first.snapshot.recipe()
        self.assertIsNone(snapshot_module.recapture(self.root, recipe, environment={"runtime": "different explicit identity"}))

    def test_warm_cache_forged_graph_narrowed_recipe_or_schema_is_never_used(self):
        for field in ("graph", "snapshot_recipe", "index_version"):
            with self.subTest(field=field):
                trusted = build(self.root)
                expected = trusted.graph.to_dict()
                with sqlite3.connect(trusted.database) as connection:
                    value = json.loads(connection.execute("SELECT value FROM meta WHERE name=?", (field,)).fetchone()[0])
                    if field == "graph":
                        value["nodes"][0]["title"] = "forged cache-only semantic assertion"
                    elif field == "snapshot_recipe":
                        value["patterns"] = [path for path in value["patterns"] if path != "src/**"]
                        value["untracked"] = []
                    else:
                        value = -1
                    connection.execute("UPDATE meta SET value=? WHERE name=?", (json.dumps(value), field))
                rebuilt = load(self.root)
                self.assertNotEqual(rebuilt.database, trusted.database)
                self.assertEqual(rebuilt.graph.to_dict(), expected)
                self.assertEqual(rebuilt.snapshot.patterns, trusted.snapshot.patterns)
                self.assertTrue(rebuilt.snapshot.verify(self.root))

    def test_warm_cache_key_rotation_and_unsafe_key_cannot_reuse_old_signatures(self):
        first = build(self.root)
        key = first.database.parent.parent.parent / ".cache-key"
        key.write_bytes(os.urandom(32))
        rotated = load(self.root)
        self.assertNotEqual(rotated.database, first.database)
        self.assertEqual(rotated.graph.to_dict(), first.graph.to_dict())
        self.assertEqual(rotated.snapshot.key, first.snapshot.key)
        with patch("validation.knowledge.index.capture", side_effect=AssertionError("unexpected rebuild after rotation")):
            self.assertEqual(load(self.root).database, rotated.database)
        key.chmod(0o644)
        with self.assertRaises(KnowledgeError):
            load(self.root)
        key.unlink()
        outside = self.root.parent / "outside-key"
        outside.write_bytes(b"not an owned cache integrity key!!")
        key.symlink_to(outside)
        with self.assertRaises((KnowledgeError, OSError)):
            load(self.root)

    def test_cache_key_atomic_concurrent_creation_uses_one_complete_private_key(self):
        cache = index_module._cache(self.root, None)
        with ThreadPoolExecutor(max_workers=4) as executor:
            keys = list(executor.map(lambda _: index_module._cache_key(self.root, cache, create=True), range(4)))
        self.assertEqual(len(set(keys)), 1)
        self.assertEqual(len(keys[0]), 32)
        self.assertEqual(list(cache.glob(".cache-key-*")), [])
        self.assertEqual((cache / ".cache-key").stat().st_mode & 0o777, 0o600)

    def test_warm_cache_read_restore_race_retries_once_then_refuses(self):
        first = build(self.root)
        original = snapshot_module._read
        modifications = []
        def observed_race(root, relative):
            item = original(root, relative)
            if relative == "src/input.rs" and not modifications:
                modifications.append(relative)
                path = Path(root) / relative
                raw = path.read_bytes()
                path.write_bytes(raw + b"// transient mutation\n")
                path.write_bytes(raw)
            return item
        with patch("validation.knowledge.snapshot._read", side_effect=observed_race), \
             patch("validation.knowledge.index.model.load_graph", side_effect=AssertionError("race must retry warm validation")):
            retried = load(self.root)
        self.assertEqual(retried.database, first.database)
        self.assertTrue(retried.snapshot.verify(self.root))
        def continuous_race(root, relative):
            item = original(root, relative)
            if relative == "src/input.rs":
                path = Path(root) / relative
                raw = path.read_bytes()
                path.write_bytes(raw + b"// transient mutation\n")
                path.write_bytes(raw)
            return item
        with patch("validation.knowledge.snapshot._read", side_effect=continuous_race):
            with self.assertRaises(SnapshotChanged):
                load(self.root)

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
