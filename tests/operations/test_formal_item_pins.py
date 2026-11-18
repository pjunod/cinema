"""Item pins: a mapped source row pins the item its anchor selects, not the file."""

from __future__ import annotations

import contextlib
import importlib.machinery
import importlib.util
import io
import json
import tempfile
import unittest
from pathlib import Path
from unittest import mock


ROOT = Path(__file__).resolve().parents[2]
LOADER = importlib.machinery.SourceFileLoader("check_formal_item_pins", str(ROOT / "scripts/check-formal"))
SPEC = importlib.util.spec_from_loader(LOADER.name, LOADER)
assert SPEC is not None and SPEC.loader is not None
formal = importlib.util.module_from_spec(SPEC)
LOADER.exec_module(formal)


# The shape of the vendored SQLite writer: one long function whose match arms
# each answer a different request, with the model mapped to one arm.
WRITER = '''\
use std::sync::Arc;

/// Applies requests one at a time.
pub fn spawn_writer(rx: Receiver<Query>) {
    loop {
        match rx.recv() {
            Query::Execute(q) => {
                let note = "} not a brace, // not a comment";
                q.tx.send(run(q.sql)).expect("oneshot tx to never be dropped");
            }
            Query::Transaction(req) => {
                let rows = apply(req.queries); // serial: one transaction at a time
                req.tx.send(rows).expect("oneshot tx to never be dropped");
            }
        }
    }
}

const STORE_TIMEOUT: Duration = Duration::from_secs(3);

impl<'a> Drop for Lease<'a> {
    fn drop(&mut self) {
        self.abort();
    }
}
'''
ARM = "Query::Transaction(req) => {"
SOURCE = "vendor/hiqlite/src/store/state_machine/sqlite/writer.rs"


def row(text: str, anchor: str, scope: str | None = None, source: str = SOURCE) -> dict[str, str]:
    return {"table": "27 vendor serial SQLite transaction", "source": source, "anchor": anchor,
            **formal.item_pin(source, text, anchor, scope)}


def check(text: str, pinned: dict[str, str], source: str = SOURCE) -> list[str]:
    return formal.check_item_pin("authority", source, text, pinned, pinned["table"])


class RustItemPinTests(unittest.TestCase):
    def test_an_edit_outside_the_anchored_item_is_not_drift(self) -> None:
        pinned = row(WRITER, ARM)
        self.assertEqual(pinned["scope"], "arm")
        self.assertEqual(pinned["item"], "fn spawn_writer > arm")
        # Tonight's case in miniature: reply handling changes in another arm,
        # a function is added above, and an unrelated constant changes.
        edited = WRITER.replace(
            'q.tx.send(run(q.sql)).expect("oneshot tx to never be dropped");',
            "send_reply(q.tx, run(q.sql), WriterReply::Execute);",
        ).replace("from_secs(3)", "from_secs(5)").replace(
            "/// Applies requests", "fn send_reply<T>(tx: Sender<T>, value: T, kind: WriterReply) {}\n\n/// Applies requests"
        )
        self.assertNotEqual(edited, WRITER)
        self.assertEqual(check(edited, pinned), [])

    def test_an_edit_inside_the_anchored_item_requires_review_and_names_it(self) -> None:
        pinned = row(WRITER, ARM)
        edited = WRITER.replace(
            'req.tx.send(rows).expect("oneshot tx to never be dropped");',
            "send_reply(req.tx, rows, WriterReply::Transaction);",
        )
        problems = check(edited, pinned)
        self.assertEqual(len(problems), 1)
        self.assertIn("authority source drift requires review", problems[0])
        self.assertIn(f"{SOURCE} :: fn spawn_writer > arm", problems[0])
        self.assertIn("27 vendor serial SQLite transaction", problems[0])

    def test_an_arm_pin_keeps_everything_but_the_other_arms_bodies(self) -> None:
        pinned = row(WRITER, ARM)
        escapes = {
            # An earlier guarded arm takes the request before the pinned arm sees it.
            "shadowing arm": WRITER.replace(
                "            " + ARM, "            Query::Transaction(req) if req.detached => { spawn(req); }\n            " + ARM
            ),
            "scrutinee": WRITER.replace("match rx.recv() {", "match rx.try_recv() {"),
            # "Serial" is a property of the loop around the match, not of the arm.
            "loop": WRITER.replace("    loop {", "    while concurrent() {"),
            "sibling pattern": WRITER.replace("Query::Execute(q) => {", "Query::Execute(q) | Query::Other(q) => {"),
            "code before the match": WRITER.replace("    loop {", "    detach_replies();\n    loop {"),
            "attribute": WRITER.replace("/// Applies requests one at a time.", "#[cfg(any())]"),
        }
        for name, edited in escapes.items():
            self.assertNotEqual(edited, WRITER, name)
            self.assertTrue(check(edited, pinned), name)
        nested = '''\
fn route(request: Request) {
    match request {
        Request::Query(query) => match query {
            Query::Read(q) => { read(q); }
            Query::Batch { id, urgent } => run(id, urgent),
            Query::Write(q) => {
                write(q);
            }
        },
        Request::Snapshot(s) => {
            snapshot(s);
        }
    }
}
'''
        write = row(nested, "Query::Write(q) => {")
        self.assertEqual(check(nested.replace("snapshot(s);", "copy(s);").replace("read(q);", "peek(q);"), write), [])
        self.assertTrue(check(nested.replace("write(q);", "write(q); flush();"), write))
        # Only another arm's body is left out; a brace in its pattern is not a body.
        self.assertTrue(check(nested.replace("Batch { id, urgent }", "Batch { id, .. }"), write))
        self.assertTrue(check(nested.replace("Request::Snapshot(s) =>", "Request::Snapshot(s) | Request::Query(_) =>"), write))

    def test_an_item_scope_pin_covers_the_whole_function(self) -> None:
        pinned = row(WRITER, ARM, "item")
        self.assertEqual(pinned["item"], "fn spawn_writer")
        edited = WRITER.replace("run(q.sql)", "run_checked(q.sql)")
        self.assertIn(f"{SOURCE} :: fn spawn_writer (", check(edited, pinned)[0])
        self.assertEqual(check(WRITER.replace("from_secs(3)", "from_secs(5)"), pinned), [])

    def test_a_renamed_or_deleted_anchor_fails(self) -> None:
        pinned = row(WRITER, ARM)
        renamed_arm = WRITER.replace(ARM, "Query::Txn(req) => {")
        self.assertIn("anchor must occur exactly once", check(renamed_arm, pinned)[0])
        deleted = WRITER[:WRITER.index("            Query::Transaction")] + WRITER[WRITER.index("        }\n    }\n}"):]
        self.assertIn("anchor must occur exactly once", check(deleted, pinned)[0])
        duplicated = WRITER.replace("    }\n}\n\nconst", "    }\n}\n\nfn other() { match x { Query::Transaction(req) => {} } }\n\nconst")
        self.assertIn("anchor must occur exactly once", check(duplicated, pinned)[0])
        # The anchor survives but the item that holds it has another name.
        renamed_item = WRITER.replace("pub fn spawn_writer(", "pub fn spawn_db_writer(")
        problem = check(renamed_item, pinned)[0]
        self.assertIn("renamed or moved", problem)
        self.assertIn("fn spawn_writer > arm is now fn spawn_db_writer > arm", problem)
        # A header anchor names the item itself, so a rename removes the anchor.
        header = row(WRITER, "const STORE_TIMEOUT: Duration = Duration::from_secs(3);")
        self.assertEqual(header["item"], "const STORE_TIMEOUT")
        self.assertIn("anchor must occur exactly once", check(WRITER.replace("STORE_TIMEOUT", "WRITE_TIMEOUT"), header)[0])

    def test_an_anchor_that_selects_no_item_never_passes(self) -> None:
        with self.assertRaisesRegex(formal.AnchorError, "not inside a named item"):
            formal.item_pin(SOURCE, WRITER, "use std::sync::Arc;")
        with self.assertRaisesRegex(formal.AnchorError, "no item extractor"):
            formal.item_pin("notes/plan.txt", "an anchor\n", "an anchor")
        with self.assertRaisesRegex(formal.AnchorError, "match arm"):
            formal.item_pin(SOURCE, WRITER, "let rows = apply(req.queries);", "arm")
        with self.assertRaisesRegex(formal.AnchorError, "match arm"):
            formal.item_pin(SOURCE, WRITER, "    loop {", "arm")
        with self.assertRaisesRegex(formal.AnchorError, "unknown pin scope"):
            formal.item_pin(SOURCE, WRITER, ARM, "file")
        pinned = row(WRITER, ARM)
        moved_out = WRITER.replace("            " + ARM, "            /* " + ARM + " */\n            Query::Renamed(req) => {")
        self.assertIn("does not select a pinnable item", check(moved_out, pinned)[0])
        self.assertIn("needs a reviewed scope and item", check(WRITER, {**pinned, "scope": None})[0])
        self.assertIn("item hash is not reviewed", check(WRITER, {**pinned, "item_sha256": "unreviewed"})[0])
        self.assertIn("retired whole-file pin", check(WRITER, {**pinned, "sha256": "a" * 64})[0])
        broken = WRITER.replace("    loop {", "    loop {{")
        self.assertIn("does not select a pinnable item", check(broken, pinned)[0])

    def test_layout_is_not_drift_but_literals_and_comment_boundaries_are(self) -> None:
        pinned = row(WRITER, ARM)
        reflowed = WRITER.replace(
            "                let rows = apply(req.queries); // serial",
            "                let rows =\n                    apply(req.queries);   // serial",
        )
        self.assertNotEqual(reflowed, WRITER)
        self.assertEqual(check(reflowed, pinned), [])
        literal = WRITER.replace('"oneshot tx to never be dropped");\n            }\n        }', '"oneshot  tx to never be dropped");\n            }\n        }')
        self.assertNotEqual(literal, WRITER)
        self.assertIn("drift requires review", check(literal, pinned)[0])
        # Joining a line onto a comment removes code; it must not hash the same.
        hidden = WRITER.replace("one transaction at a time\n                req.tx", "one transaction at a time req.tx")
        self.assertIn("drift requires review", check(hidden, pinned)[0])

    def test_default_scope_takes_a_region_only_where_one_is_self_contained(self) -> None:
        source = '''\
impl Worker {
    async fn run(&self) {
        let heartbeat = tokio::spawn(async move {
            tick().await;
        });
        for attempt in 0..4 {
            retry(attempt);
        }
        if queue_loss.is_ready() {
            return;
        }
        let Some(token) = self.token.take() else {
            return;
        };
        let reply = Reply::Released {
            ok: true,
        };
        let results = match transaction {
            Ok(rows) => {
                rows
            }
        };
        let publisher = lease.publisher(self.store.as_ref());
        publish(publisher, results);
    }
}
'''
        expect = {
            "let heartbeat = tokio::spawn(async move {": "item",
            "for attempt in 0..4 {": "item",
            "let results = match transaction {": "item",
            "Ok(rows) => {": "arm",
            "if queue_loss.is_ready() {": "item",
            "let Some(token) = self.token.take() else {": "item",
            "let reply = Reply::Released {": "item",
            "async fn run(&self) {": "item",
            "impl Worker {": "item",
        }
        for anchor, scope in expect.items():
            pin = formal.item_pin("src/worker.rs", source, anchor)
            self.assertEqual(pin["scope"], scope, anchor)
            self.assertTrue(pin["item"].startswith("impl Worker"), anchor)
        self.assertEqual(formal.item_pin("src/worker.rs", source, "impl Worker {")["item"], "impl Worker")
        self.assertEqual(formal.item_pin("src/worker.rs", source, "retry(attempt);")["item"], "impl Worker > fn run")
        # A `let x = match …` only produces a value: what is done with it comes
        # after the block, so swapping the fenced publisher must be drift.
        lease = {"table": "genres", "source": "src/worker.rs", "anchor": "let results = match transaction {",
                 **formal.item_pin("src/worker.rs", source, "let results = match transaction {")}
        swapped = source.replace("lease.publisher(self.store.as_ref())", "PublicationStore::unfenced(self.store.as_ref())")
        self.assertIn("src/worker.rs :: impl Worker > fn run", check(swapped, lease, "src/worker.rs")[0])

    def test_a_cfg_on_an_enclosing_item_is_part_of_the_label_and_the_hash(self) -> None:
        source = "impl JobFence {\n    async fn renew(&self) -> bool {\n        true\n    }\n}\n"
        pinned = row(source, "async fn renew(&self) -> bool {", source="src/jobs.rs")
        self.assertEqual(pinned["item"], "impl JobFence > fn renew")
        compiled_out = "#[cfg(any())]\n" + source
        self.assertEqual(
            formal.item_pin("src/jobs.rs", compiled_out, pinned["anchor"])["item"], "#[cfg(any())] impl JobFence > fn renew"
        )
        self.assertIn("renamed or moved", check(compiled_out, pinned, "src/jobs.rs")[0])
        self.assertNotEqual(formal.item_pin("src/jobs.rs", compiled_out, pinned["anchor"])["item_sha256"], pinned["item_sha256"])
        inner = source.replace("impl JobFence {\n", "impl JobFence {\n    #![cfg(feature = \"x\")]\n")
        self.assertIn("renamed or moved", check(inner, pinned, "src/jobs.rs")[0])
        whole_file = "#![cfg(unix)]\n" + source
        self.assertIn("renamed or moved", check(whole_file, pinned, "src/jobs.rs")[0])
        on_item = source.replace("    async fn renew", "    #[cfg(test)]\n    async fn renew")
        self.assertEqual(formal.item_pin("src/jobs.rs", on_item, pinned["anchor"])["item"], "impl JobFence > #[cfg(test)] fn renew")
        self.assertEqual(formal.rust_function(on_item, "renew")[0], "impl JobFence > #[cfg(test)] fn renew")

    def test_item_keywords_inside_types_and_literals_do_not_start_items(self) -> None:
        source = r'''
#[derive(Debug)]
pub(crate) struct Holder<'a, const N: usize> {
    name: &'static str,
    raw: *const u8,
    brace: char,
}

/* outer /* nested */ fn ghost() { */
pub(in crate::store) async fn fenced_call<'call, T>(
    reader: impl Read + 'call,
    quote: char,
) -> impl Future<Output = T> + 'call
where
    T: Send,
{
    let open = '{';
    let escaped = '\'';
    let sql = r#"SELECT "fn fake() {" FROM t; }"#;
    let label = b"}";
    marker(open, escaped, sql, label)
}

const BIND_SQL: &str = r#"
UPDATE jobs SET state = 'bound' WHERE id = $1;
"#;

pub const fn limit() -> usize { 4 }
'''
        cases = {
            "raw: *const u8,": "struct Holder",
            "name: &'static str,": "struct Holder",
            "#[derive(Debug)]": "struct Holder",
            "reader: impl Read + 'call,": "fn fenced_call",
            "marker(open, escaped, sql, label)": "fn fenced_call",
            "T: Send,": "fn fenced_call",
            "UPDATE jobs SET state = 'bound' WHERE id = $1;": "const BIND_SQL",
            "pub const fn limit() -> usize { 4 }": "fn limit",
        }
        for anchor, item in cases.items():
            self.assertEqual(formal.item_pin("src/store.rs", source, anchor)["item"], item, anchor)
        # A comment belongs to the item it introduces; it never starts one.
        self.assertEqual(formal.item_pin("src/store.rs", source, "fn ghost()")["item"], "fn fenced_call")
        with self.assertRaisesRegex(formal.AnchorError, "not inside a named item"):
            formal.item_pin("src/store.rs", source + "// fn trailing() {}\n", "fn trailing()")

    def test_a_conformance_test_is_pinned_by_its_function(self) -> None:
        source = "mod tests {\n    #[tokio::test]\n    async fn worker_schedule() { step(); }\n    fn helper() {}\n}\n"
        label, canonical = formal.rust_function(source, "worker_schedule")
        self.assertEqual(label, "mod tests > fn worker_schedule")
        self.assertIn("#[tokio::test]", canonical)
        self.assertEqual(canonical, formal.rust_function(source.replace("fn helper() {}", "fn helper() { more(); }"), "worker_schedule")[1])
        self.assertNotEqual(canonical, formal.rust_function(source.replace("step();", "step(); step();"), "worker_schedule")[1])
        with self.assertRaisesRegex(formal.AnchorError, "exactly one fn"):
            formal.rust_function(source, "absent")
        with self.assertRaisesRegex(formal.AnchorError, "exactly one fn"):
            formal.rust_function(source + "fn helper() {}\n", "helper")


class OtherLanguageItemPinTests(unittest.TestCase):
    def test_sql_pins_one_statement_and_a_trigger_runs_to_its_own_end(self) -> None:
        source = '''\
-- Queue schema.
CREATE TABLE IF NOT EXISTS jobs (id INTEGER PRIMARY KEY, note TEXT DEFAULT 'a;b');

CREATE TRIGGER IF NOT EXISTS job_completed AFTER UPDATE ON jobs
WHEN CASE WHEN NEW.state = 'done' THEN 1 ELSE 0 END = 1
BEGIN
    UPDATE waiters SET state = CASE WHEN NEW.ok THEN 'succeeded' ELSE 'failed' END WHERE job_id = NEW.id;
    DELETE FROM leases WHERE job_id = NEW.id;
END;

CREATE VIEW IF NOT EXISTS required AS
SELECT id, 'provider:tmdb' FROM jobs WHERE kind IN ('library_scan','metadata_refresh');
'''
        path = "crates/plurx-core/src/store/jobs.sql"
        trigger = row(source, "DELETE FROM leases WHERE job_id = NEW.id;", source=path)
        self.assertEqual(trigger["item"], "trigger job_completed")
        view = row(source, "SELECT id, 'provider:tmdb' FROM jobs", source=path)
        self.assertEqual(view["item"], "view required")
        self.assertEqual(row(source, "note TEXT DEFAULT 'a;b'", source=path)["item"], "table jobs")
        outside = source.replace("note TEXT", "detail TEXT").replace("'library_scan',", "'library_scan', 'dv_title_scan',")
        self.assertEqual(check(outside, trigger, path), [])
        self.assertIn(f"{path} :: view required", check(outside, view, path)[0])
        inside = source.replace("ELSE 'failed' END", "ELSE 'cancelled' END")
        self.assertIn(f"{path} :: trigger job_completed", check(inside, trigger, path)[0])
        self.assertEqual(check(inside, view, path), [])
        renamed = source.replace("TRIGGER IF NOT EXISTS job_completed", "TRIGGER IF NOT EXISTS job_settled")
        self.assertIn("renamed or moved", check(renamed, trigger, path)[0])
        # A later statement that drops or replaces the object decides what is installed.
        dropped = source + "DROP TRIGGER IF EXISTS job_completed;\n"
        self.assertIn(f"{path} :: trigger job_completed", check(dropped, trigger, path)[0])
        self.assertEqual(check(dropped, view, path), [])
        replaced = dropped + "CREATE TRIGGER job_completed AFTER UPDATE ON jobs BEGIN SELECT 1; END;\n"
        self.assertNotEqual(
            formal.item_pin(path, replaced, trigger["anchor"])["item_sha256"],
            formal.item_pin(path, dropped, trigger["anchor"])["item_sha256"],
        )
        self.assertEqual(check(source + "DROP TRIGGER IF EXISTS another_trigger;\n", trigger, path), [])

    def test_lockfile_pins_the_package_and_manifest_pins_the_key(self) -> None:
        lock = '''\
version = 4

[[package]]
name = "tokio"
version = "1.53.1"
checksum = "aaaa"
dependencies = [
 "mio",
]

[[package]]
name = "wasmtime"
version = "49.0.2"
checksum = "bbbb"
'''
        anchor = 'name = "tokio"\nversion = "1.53.1"'
        pinned = row(lock, anchor, source="Cargo.lock")
        self.assertEqual(pinned["item"], "[[package]] tokio 1.53.1")
        self.assertEqual(check(lock.replace("49.0.2", "50.0.0").replace("bbbb", "cccc"), pinned, "Cargo.lock"), [])
        self.assertIn("Cargo.lock :: [[package]] tokio 1.53.1", check(lock.replace("aaaa", "dddd"), pinned, "Cargo.lock")[0])
        self.assertIn("exactly once", check(lock.replace("1.53.1", "1.54.0"), pinned, "Cargo.lock")[0])
        # An anchor on one key of a package still pins the whole entry.
        single = row(lock, 'name = "tokio"', source="Cargo.lock")
        self.assertEqual((single["item"], single["item_sha256"]), (pinned["item"], pinned["item_sha256"]))
        manifest = '[workspace.dependencies]\nhiqlite = { version = "=0.14.0", features = [\n    "sqlite",\n] }\ntokio = "1"\n'
        key = row(manifest, 'hiqlite = { version = "=0.14.0"', source="Cargo.toml")
        self.assertEqual(key["item"], "[workspace.dependencies] > hiqlite")
        self.assertEqual(check(manifest.replace('tokio = "1"', 'tokio = "1.53"'), key, "Cargo.toml"), [])
        self.assertIn("drift requires review", check(manifest.replace('"sqlite"', '"sqlite", "cache"'), key, "Cargo.toml")[0])

    def test_python_pins_the_function(self) -> None:
        source = "#!/usr/bin/env python3\nimport os\n\n\ndef validated_challenge(data_dir, node):\n    return node\n\n\ndef other():\n    return 1\n"
        path = "scripts/source-custody-recovery"
        pinned = row(source, "def validated_challenge(data_dir, node):", source=path)
        self.assertEqual(pinned["item"], "def validated_challenge")
        self.assertEqual(check(source.replace("return 1", "return 2"), pinned, path), [])
        self.assertIn(f"{path} :: def validated_challenge", check(source.replace("return node", "return node.strip()"), pinned, path)[0])
        with self.assertRaisesRegex(formal.AnchorError, "Python function or class"):
            formal.item_pin(path, source, "import os")


def mapped_rows() -> list[tuple[str, str, dict[str, str]]]:
    """Every pinned source row of the three packages: (package, file, row)."""
    rows = []
    for package in ("durable-jobs", "operation-progress"):
        mapping = json.loads((ROOT / f"verification/{package}/mapping.json").read_text(encoding="utf-8"))
        rows += [(package, entry["source"], entry) for entry in mapping["source_mapping"]["tables"]]
    correspondence = json.loads((ROOT / "verification/worker-refinement/correspondence.json").read_text(encoding="utf-8"))
    rows += [("worker-refinement", entry["source"]["file"], entry["source"]) for entry in correspondence["actions"]]
    rows += [("worker-refinement", entry["file"], entry) for entry in correspondence["dependencies"]]
    return rows


class ReviewedEscapeTests(unittest.TestCase):
    """Edits the review of the first item-pin migration got past `check-mapping`.

    Each names a function or constant in the repository's own source that
    carries part of a mapped claim. The test fails if no row pins it any more,
    or if an edit at the end of its body passes the check. It does not depend on
    what the item says, so reviewed changes to it do not break this test.
    """

    ESCAPES = (
        # A `let lease = match …` block pin left the publisher after it unpinned.
        ("durable-jobs", "crates/plurxd/src/state.rs", "fn genre_backfill_pass"),
        ("durable-jobs", "crates/plurxd/src/state.rs", "fn artwork_retry_pass"),
        ("worker-refinement", "crates/plurxd/src/background_jobs.rs", "fn start"),
        # The claim lives in a same-file function or constant the pinned item reads.
        ("operation-progress", "crates/plurx-core/src/cluster/membership.rs", "fn decide_cluster_job_authority"),
        ("operation-progress", "crates/plurx-core/src/cluster/membership.rs", "fn local_node_is_committed_voter"),
        ("operation-progress", "crates/plurxd/src/job_lease.rs", "const RELEASE_ATTEMPTS"),
        ("operation-progress", "crates/plurxd/src/job_lease.rs", "const RELEASE_RETRY_DELAY"),
        ("worker-refinement", "crates/plurxd/src/job_lease.rs", "const RELEASE_ATTEMPTS"),
        ("operation-progress", "crates/plurxd/src/job_lease.rs", "fn lease_time_remaining"),
        ("operation-progress", "vendor/hiqlite/src/store/state_machine/sqlite/writer.rs", "fn publish_then_send"),
        ("operation-progress", "vendor/hiqlite/src/store/state_machine/sqlite/writer.rs", "fn is_exact_source_layout_guard"),
        ("operation-progress", "vendor/hiqlite/src/store/state_machine/sqlite/writer.rs", "fn source_layout_rollback_result"),
        ("operation-progress", "vendor/hiqlite/src/store/state_machine/sqlite/writer.rs", "fn check_stmt_params_count"),
        ("durable-jobs", "crates/plurx-core/src/store/hiqlite_publication.rs", "const ATOMIC_PUBLICATION_TTL_MS"),
        ("durable-jobs", "crates/plurx-core/src/store/hiqlite_publication.rs", "fn fence_rejected"),
        ("durable-jobs", "crates/plurx-core/src/store/hiqlite_publication.rs", "fn publication_row_id"),
        ("durable-jobs", "crates/plurx-core/src/store/hiqlite_publication.rs", "fn lease_i64"),
        ("operation-progress", "crates/plurx-core/src/store/hiqlite_publication.rs", "fn fence_rejected"),
        ("operation-progress", "crates/plurxd/src/background_jobs.rs", "fn claim_with_resolution"),
        ("operation-progress", "crates/plurxd/src/background_jobs.rs", "fn library_candidates"),
        ("operation-progress", "crates/plurxd/src/background_jobs.rs", "fn settle"),
        ("operation-progress", "crates/plurxd/src/background_jobs.rs", "fn unix_ms"),
        ("worker-refinement", "crates/plurxd/src/background_jobs.rs", "fn claim_with_resolution"),
        # `may_execute_job` is a provided method of the authority trait.
        ("operation-progress", "crates/plurx-core/src/cluster/coordination.rs", "trait ClusterJobAuthority"),
        ("worker-refinement", "crates/plurx-core/src/cluster/coordination.rs", "trait ClusterJobAuthority"),
        # Main extracted these from the formerly pinned claim statement/impl.
        ("durable-jobs", "crates/plurx-core/src/store/background_jobs.rs", "const CLAIM_SQL"),
        ("durable-jobs", "crates/plurx-core/src/store/background_jobs.rs", "fn claim_attempt"),
        ("durable-jobs", "crates/plurx-core/src/store/background_jobs.rs", "fn local_claim_verdict"),
        ("durable-jobs", "crates/plurx-core/src/store/background_jobs.rs", "const CLAIM_VERDICT_SQL"),
        ("durable-jobs", "crates/plurx-core/src/store/background_jobs.rs", "macro_rules! claim_identity"),
        ("durable-jobs", "crates/plurx-core/src/store/background_jobs.rs", "macro_rules! claim_subtitle_extract_demand"),
        ("durable-jobs", "crates/plurx-core/src/store/background_jobs.rs", "macro_rules! claim_kind_demand"),
        ("durable-jobs", "crates/plurx-core/src/store/background_jobs.rs", "macro_rules! claim_state"),
        ("durable-jobs", "crates/plurx-core/src/store/background_jobs.rs", "macro_rules! claim_attempt_ledger"),
        ("durable-jobs", "crates/plurx-core/src/store/background_jobs.rs", "macro_rules! claim_foreground_window"),
        ("durable-jobs", "crates/plurx-core/src/store/background_jobs.rs", "macro_rules! claim_source_holders"),
        ("durable-jobs", "crates/plurx-core/src/store/background_jobs.rs", "macro_rules! claim_maintenance_reader"),
        # Fixtures the conformance tests stand on.
        ("worker-refinement", "crates/plurxd/src/library_work.rs", "fn spawn_worker"),
        ("worker-refinement", "crates/plurx-core/tests/store_contract/formal_publication.rs", "fn enqueue_and_claim_library_work"),
    )

    def test_each_escape_is_pinned_and_an_edit_inside_it_is_drift(self) -> None:
        rows = mapped_rows()
        for package, source, tail in self.ESCAPES:
            with self.subTest(package=package, source=source, item=tail):
                pinned = [
                    entry for owner, file, entry in rows
                    if owner == package and file == source and entry["scope"] == "item"
                    and entry["item"].split(" > ")[-1].split(" ")[-2:] == tail.split(" ")
                ]
                self.assertTrue(pinned, "no item row pins this any more")
                text = (ROOT / source).read_text(encoding="utf-8")
                _, _, _, items, _ = formal._rust_index(text)
                at = text.index(pinned[0]["anchor"])
                end = formal._enclosing(items, at, at + len(pinned[0]["anchor"]))[-1]["end"]
                edited = text[:end - 1] + " /* edited */ " + text[end - 1:]
                current = {**pinned[0], **formal.item_pin(source, text, pinned[0]["anchor"], "item")}
                problems = formal.check_item_pin("authority", source, edited, current, "escape")
                self.assertEqual(len(problems), 1)
                if text.count("\n", at, end):
                    self.assertIn(f"drift requires review: {source} :: {current['item']}", problems[0])
                else:  # a one-line constant is its own anchor, so editing it removes the anchor
                    self.assertIn("anchor must occur exactly once", problems[0])

    def test_the_serial_writer_row_sees_its_loop_and_the_arms_around_it(self) -> None:
        source = "vendor/hiqlite/src/store/state_machine/sqlite/writer.rs"
        pinned = [entry for owner, file, entry in mapped_rows()
                  if owner == "operation-progress" and file == source and entry["scope"] == "arm"]
        self.assertEqual([entry["item"] for entry in pinned], ["fn spawn_writer > arm"])
        text = (ROOT / source).read_text(encoding="utf-8")
        anchor = pinned[0]["anchor"]
        current = {**pinned[0], **formal.item_pin(source, text, anchor, "arm")}
        line = text.rfind("\n", 0, text.index(anchor)) + 1
        shadowed = text[:line] + "Query::Transaction(req) if req.queries.is_empty() => { drop(req); }\n" + text[line:]
        self.assertTrue(formal.check_item_pin("authority", source, shadowed, current, "27"))
        body = text.index("{", text.index("pub fn spawn_writer("))
        self.assertTrue(formal.check_item_pin(
            "authority", source, text[:body + 1] + " let detached = true; " + text[body + 1:], current, "27"))


class ItemPinMappingTests(unittest.TestCase):
    def test_a_whole_file_mapping_is_refused(self) -> None:
        mapping = {"source_mapping": {"status": "reviewed", "tables": [
            {"table": "t", "source": "src.rs", "anchor": "a", "sha256": "a" * 64},
        ]}}
        drift, _ = formal.mapping_inputs(mapping, "operation-progress")
        self.assertTrue(any("whole-file pins are retired" in item for item in drift))

    def test_item_pins_reports_stale_pins_and_writes_only_on_request(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            mapping_path = root / "verification/operation-progress/mapping.json"
            mapping_path.parent.mkdir(parents=True)
            (root / SOURCE).parent.mkdir(parents=True)
            (root / SOURCE).write_text(WRITER, encoding="utf-8")
            mapping_path.write_text(json.dumps({"source_mapping": {"status": "reviewed", "tables": [
                {"table": "27 vendor serial SQLite transaction", "source": SOURCE, "anchor": ARM, "sha256": "a" * 64},
            ]}}), encoding="utf-8")

            def run(write: bool) -> tuple[int, dict[str, object]]:
                output = io.StringIO()
                with mock.patch.object(formal, "ROOT", root), contextlib.redirect_stdout(output):
                    code = formal.item_pins("operation-progress", write)
                return code, json.loads(output.getvalue())

            code, report = run(False)
            self.assertEqual((code, report["written"]), (1, False))
            self.assertIn("sha256", json.loads(mapping_path.read_text())["source_mapping"]["tables"][0])
            code, report = run(True)
            self.assertEqual((code, report["written"]), (0, True))
            written = json.loads(mapping_path.read_text())["source_mapping"]
            self.assertEqual(written["pin_scheme"], formal.PIN_SCHEME)
            self.assertEqual(written["tables"][0], row(WRITER, ARM))
            self.assertEqual(run(False), (0, {"package": "operation-progress", "changes": [], "failures": [], "written": False}))
            (root / SOURCE).write_text(WRITER.replace("apply(req.queries)", "apply_all(req.queries)"), encoding="utf-8")
            code, report = run(False)
            self.assertEqual(code, 1)
            self.assertEqual(report["changes"][0]["row"], "27 vendor serial SQLite transaction")
            (root / SOURCE).write_text(WRITER.replace(ARM, "Query::Txn(req) => {"), encoding="utf-8")
            code, report = run(True)
            self.assertEqual((code, report["written"]), (2, False))
            self.assertIn("exactly once", report["failures"][0])


if __name__ == "__main__":
    unittest.main()
