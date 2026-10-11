#!/usr/bin/env python3
"""Run unchanged historical Store code against the candidate ownership rebuild."""
import argparse
import hashlib
import io
import os
import re
from pathlib import Path
import subprocess
import tarfile

BASELINE = "971265536a259dea38b0f7a9a8752a5a74e8c025"
ROOT = Path(__file__).resolve().parent.parent

SQLITE_TEST = r"""
#[tokio::test]
async fn sharing_old_store_sqlite_compatibility() {
    for pooled in [false, true] {
        for rebuilt in [false, true] {
            let root = tempfile::tempdir().expect("fixture directory");
            let store = if pooled {
                super::SqliteStore::open_with_read_connections(&root.path().join("probe.db"), 2)
            } else {
                super::SqliteStore::open_in_memory()
            }.expect("old Store initialization");
            store.with_conn(move |conn| {
                conn.execute_batch(include_str!("../../../tests/fixtures/session-principal-local.sql"))?;
                if rebuilt {
                    conn.execute_batch("BEGIN IMMEDIATE")?;
                    conn.execute_batch(include_str!("../../../tests/fixtures/sharing-old-store-rebuild.sql"))?;
                    conn.execute_batch("COMMIT")?;
                }
                Ok(())
            }).await.expect("seed and rebuild actual old Store connection");
            exercise_old_store(&store, if pooled { "sqlite-pooled" } else { "sqlite-memory" }, rebuilt).await;
        }
    }
}
"""

HIQLITE_TEST = r"""
#[cfg(feature = "hiqlite-contract-tests")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sharing_old_store_three_voter_compatibility() {
    use super::*;
    let _case = HIQLITE_CASE.lock().await;
    for rebuilt in [false, true] {
        let cluster = ContractCluster::start().await;
        let store = open_contract_hiqlite_store(&cluster).await;
        let client = Client::remote(cluster.addresses.clone(), true, true,
            CONTRACT_API_SECRET.to_owned(), false, None).await.expect("fixture client");
        for result in client.batch(include_str!("fixtures/session-principal-local.sql")).await.expect("fixture batch") {
            result.expect("seed old actual replicated schema");
        }
        if rebuilt {
            let statements: Vec<(String, hiqlite::Params)> = include_str!("fixtures/sharing-old-store-rebuild.sql")
                .split("-- next statement\n")
                .map(|sql| (sql.trim().trim_end_matches(';').to_owned(), hiqlite::params!())).collect();
            for result in client.txn(statements).await.expect("candidate transaction") {
                result.expect("rebuild transaction statement");
            }
        }
        exercise_old_store(&store, "hiqlite-three-voters", rebuilt).await;
        drop(store);
    }
}
"""

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-dir", type=Path, required=True, help="new, disposable extraction directory")
    parser.add_argument("--target-dir", type=Path, required=True, help="dedicated warm Cargo target directory")
    args = parser.parse_args()
    if args.source_dir.exists():
        parser.error("source-dir must not exist; never overlay user source")
    version = subprocess.check_output(["rustc", "--version"], text=True).strip()
    if not version.startswith("rustc 1.97.1 "):
        parser.error(f"repository-pinned Rust 1.97.1 required; got {version}")
    source = subprocess.check_output(["git", "archive", "--format=tar", BASELINE], cwd=ROOT)
    args.source_dir.mkdir(parents=True)
    with tarfile.open(fileobj=io.BytesIO(source)) as archive:
        archive.extractall(args.source_dir, filter="data")
    candidate_commit = subprocess.check_output(
        ["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    candidate_files = [
        "crates/plurx-core/tests/fixtures/session-principal-local.sql",
        "crates/plurx-core/tests/fixtures/sharing-old-store-probe.rs",
        "crates/plurx-core/src/store/session_principal_rebuild.sql",
    ]
    candidate_archive = subprocess.check_output(
        ["git", "archive", "--format=tar", candidate_commit, *candidate_files], cwd=ROOT)
    with tarfile.open(fileobj=io.BytesIO(candidate_archive)) as archive:
        committed = {name: archive.extractfile(name).read() for name in candidate_files}
    core = args.source_dir / "crates/plurx-core"
    fixtures = core / "tests/fixtures"
    for name in ["session-principal-local.sql", "sharing-old-store-probe.rs"]:
        (fixtures / name).write_bytes(committed["crates/plurx-core/tests/fixtures/" + name])
    local_fixture = fixtures / "session-principal-local.sql"
    local_fixture.write_text(local_fixture.read_text().replace(
        "'live'", "'00000000-0000-4000-a000-000000000072'").replace(
        "session:live", "session:00000000-0000-4000-a000-000000000072"))
    # The retention fixture deliberately starts pending/draining. For old
    # active-inventory probes, settle its original local row before rebuilding.
    with (fixtures / "session-principal-local.sql").open("a") as out:
        out.write("\nUPDATE media_session_requests SET state='resolved', response_json='{}' WHERE request_id='request';\nUPDATE media_sessions SET drain_deadline_ms=NULL, session_id='00000000-0000-4000-a000-000000000071' WHERE incarnation_id='00000000-0000-4000-a000-000000000072';\n")
    rebuild = committed["crates/plurx-core/src/store/session_principal_rebuild.sql"]
    (fixtures / "sharing-old-store-rebuild.sql").write_bytes(rebuild)
    probe = (fixtures / "sharing-old-store-probe.rs").read_text()
    (core / "src/store/sqlite/sharing_old_store_probe.rs").write_text(
        probe.replace("use plurx_core::", "use crate::") + SQLITE_TEST)
    with (core / "src/store/sqlite/mod.rs").open("a") as out:
        out.write('\n#[cfg(test)]\nmod sharing_old_store_probe;\n')
    # Append only qualification code to the historical integration harness.
    # Production Store methods and production SQL remain byte-for-byte old.
    with (core / "tests/store_contract.rs").open("a") as out:
        out.write("\nmod sharing_old_store_qualification {\n" + probe + HIQLITE_TEST + "\n}\n")
    print(f"Baseline production source: {BASELINE}", flush=True)
    print("Historical source archive SHA256: " + hashlib.sha256(source).hexdigest(), flush=True)
    print("Runner SHA256: " + hashlib.sha256(Path(__file__).read_bytes()).hexdigest(), flush=True)
    print("Applied local fixture SHA256: " + hashlib.sha256(local_fixture.read_bytes()).hexdigest(), flush=True)
    print("Qualification harness SHA256: " + hashlib.sha256(probe.encode()).hexdigest(), flush=True)
    print("Candidate source commit: " + candidate_commit, flush=True)
    print("Candidate archive SHA256: " + hashlib.sha256(candidate_archive).hexdigest(), flush=True)
    print(f"Candidate rebuild SHA256: {hashlib.sha256(rebuild).hexdigest()}", flush=True)
    print(version, flush=True)
    env = dict(os.environ, CARGO_TARGET_DIR=str(args.target_dir.resolve()), CARGO_INCREMENTAL="0", CARGO_BUILD_JOBS="2")
    commands = [
        ["cargo", "test", "--locked", "--offline", "-p", "plurx-core", "--features", "hiqlite-store", "--lib", "sharing_old_store_sqlite_compatibility", "--", "--nocapture"],
        ["cargo", "test", "--locked", "--offline", "-p", "plurx-core", "--features", "hiqlite-contract-tests", "--test", "store_contract", "sharing_old_store_three_voter_compatibility", "--", "--nocapture"],
    ]
    for command in commands:
        print("Executing: " + " ".join(command), flush=True)
        process = subprocess.Popen(command, cwd=args.source_dir, env=env,
                                   stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        passed = False
        for line in process.stdout:
            print(line, end="", flush=True)
            passed |= bool(re.search(r"test result: ok\. 1 passed; 0 failed; 0 ignored;", line))
        if process.wait() != 0 or not passed:
            raise RuntimeError("qualification must execute exactly one passing test with zero ignored")

if __name__ == "__main__":
    main()
