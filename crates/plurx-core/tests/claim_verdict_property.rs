//! Differential test of the claim local verdict against the claim statement
//! (docs/cluster/CLAIM-LOCAL-VERDICT-BUILD.md §5.2, adversarial review).
//!
//! Random queue states on the SQLite schema, each inside a savepoint. For
//! every state: the verdict admits exactly when `CLAIM_SQL` updates one row,
//! a row the verdict leaves out is one the statement refuses, and the job is
//! in the admissible listing exactly when the statement would admit a claim
//! at its current revision. No dispatch turn is ever advertised, so the
//! dispatch-turn trigger (which the verdict does not pre-evaluate) stays out.
#![cfg(feature = "fixtures")]

use plurx_core::store::background_jobs::claim_sql_fixture::{
    candidate_listing_sql, CLAIM_SQL, CLAIM_VERDICT_SQL,
};
use plurx_core::store::background_jobs::{BackgroundJobStore, EnqueueJob, JobPayload, JobRequest};
use plurx_core::store::SqliteStore;
use std::fmt::Write;

const NOW: i64 = 1_000_000;
/// Cases in an ordinary run. Each is a real statement inside a savepoint, a
/// quarter of a second on a small machine, so the lane runs enough to reach
/// every outcome (the tally at the end asserts that) and no more. Set
/// `PLURX_CLAIM_PROPERTY_CASES` for a deeper run; 2,000 passed when this was
/// written.
const DEFAULT_CASES: usize = 400;

fn cases() -> usize {
    std::env::var("PLURX_CLAIM_PROPERTY_CASES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_CASES)
}

/// xorshift64*: a fixed seed, no dependency.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[(self.next() % items.len() as u64) as usize]
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
}

fn fragment_job(file_id: i64) -> EnqueueJob {
    EnqueueJob {
        id: uuid::Uuid::new_v4().to_string(),
        payload: JobPayload::FragmentIndexBuild {
            file_id,
            source_generation: format!("source:{file_id}"),
            source_size: 100,
            source_mtime: 1,
            source_sha256: "d".repeat(64),
            cache_key: format!("{file_id:0>64}"),
            pipeline_digest: "a".repeat(64),
        },
        dedupe_key: format!("fragment:{file_id}"),
        priority: 1,
        not_before_ms: NOW - 1_000,
        now_ms: NOW - 1_000,
        request: JobRequest {
            scope: "user:1".to_owned(),
            request_id: uuid::Uuid::new_v4().to_string(),
            request_digest: "b".repeat(64),
            consumer_kind: "analysis".to_owned(),
            consumer_ref: format!("analysis:{file_id}"),
            target_node_id: None,
            deadline_ms: None,
            retain_identity: false,
        },
    }
}

fn exec(db: &rusqlite::Connection, state: &mut String, sql: String) {
    let _ = writeln!(state, "{sql};");
    db.execute_batch(&sql)
        .unwrap_or_else(|error| panic!("state statement failed: {error}\n{state}"));
}

fn claim_body(job: &str, kind: &str, revision: i64, now: i64) -> String {
    serde_json::json!({
        "job_id": job, "expected_revision": revision, "node_id": "node-a",
        "boot_id": uuid::Uuid::new_v4().to_string(),
        "claim_id": uuid::Uuid::new_v4().to_string(),
        "kind": kind, "payload_version": 1, "now_ms": now, "dispatched_at_ms": now,
    })
    .to_string()
}

/// Whether `CLAIM_SQL` updates the row, rolled back.
fn statement_admits(db: &rusqlite::Connection, body: &str) -> usize {
    db.execute_batch("SAVEPOINT statement").expect("savepoint");
    let updated = {
        let mut statement = db
            .prepare(&format!("{CLAIM_SQL} RETURNING id"))
            .expect("claim statement");
        let mut rows = statement.query([body]).expect("claim");
        let mut count = 0;
        while rows.next().expect("claim row").is_some() {
            count += 1;
        }
        count
    };
    db.execute_batch("ROLLBACK TO statement; RELEASE statement")
        .expect("rollback");
    updated
}

fn verdict(db: &rusqlite::Connection, body: &str) -> Option<serde_json::Value> {
    let mut statement = db.prepare(CLAIM_VERDICT_SQL).expect("verdict");
    let mut rows = statement.query([body]).expect("verdict query");
    rows.next().expect("verdict row").map(|row| {
        serde_json::from_str(&row.get::<_, String>(0).expect("verdict text")).expect("verdict json")
    })
}

fn listed(db: &rusqlite::Connection, job: &str, kind: &str, now: i64) -> bool {
    let body = serde_json::json!({
        "node_id": "node-a", "kinds": [kind], "after": null, "now_ms": now,
        "limit": 128, "admissible_only": true,
    })
    .to_string();
    let mut statement = db.prepare(&candidate_listing_sql(true)).expect("listing");
    let mut rows = statement.query([body]).expect("listing query");
    while let Some(row) = rows.next().expect("listing row") {
        let value: serde_json::Value =
            serde_json::from_str(&row.get::<_, String>(0).expect("text")).expect("json");
        if value["job"]["id"] == job {
            return true;
        }
    }
    false
}

#[tokio::test]
async fn the_verdict_and_the_admissible_listing_agree_with_the_claim_statement() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("property.db");
    let store = SqliteStore::open(&path).expect("store");
    let mut jobs = Vec::new();
    for n in 1..=4 {
        let job = fragment_job(n);
        jobs.push(job.id.clone());
        store.enqueue_job(job).await.expect("enqueue");
    }
    drop(store);
    let db = rusqlite::Connection::open(&path).expect("connection");
    let target = jobs[0].clone();
    let others = &jobs[1..];
    let mut rng = Rng(0x5EED_CAFE_F00D_0001);
    let mut tally = std::collections::BTreeMap::<String, usize>::new();
    let cases = cases();
    for case in 0..cases {
        let mut state = String::new();
        db.execute_batch("SAVEPOINT case_state")
            .expect("case savepoint");
        let now = NOW + (rng.next() % 1_000) as i64;
        let subtitle = rng.chance(15);
        let kind = if subtitle {
            "subtitle_extract"
        } else {
            "fragment_index_build"
        };
        // The job row.
        match *rng.pick(&["queued", "queued", "queued", "queued", "expired", "live"]) {
            "expired" => exec(&db, &mut state, format!(
                "UPDATE background_jobs SET state='running', owner_node_id='node-b', owner_boot_id='boot-b', \
                 claim_id='claim-b', fence=fence+1, revision=revision+1, lease_expires_ms={} WHERE id='{target}'", now - 1)),
            "live" => exec(&db, &mut state, format!(
                "UPDATE background_jobs SET state='running', owner_node_id='node-b', owner_boot_id='boot-b', \
                 claim_id='claim-b', fence=fence+1, revision=revision+1, lease_expires_ms={} WHERE id='{target}'", now + 10_000)),
            _ => {}
        }
        // The kind last: its own triggers guard state changes, not kind.
        if subtitle {
            exec(&db, &mut state, format!(
                "UPDATE background_jobs SET kind='subtitle_extract', payload_json=json_object('kind','subtitle_extract',\
                 'file_id',1,'source_generation','missing-request','track',NULL,'pipeline_digest','{}') WHERE id='{target}'",
                "a".repeat(64)));
        }
        let retry = *rng.pick(&[0, 0, 0, 0, 0, now - 1, now + 1_000]);
        let not_before = *rng.pick(&[now - 10, now - 10, now - 10, now - 10, now + 10]);
        let target_node = *rng.pick(&["NULL", "NULL", "NULL", "'node-a'", "'node-b'"]);
        let failed = *rng.pick(&[0, 0, 0, 0, 4, 5]);
        exec(
            &db,
            &mut state,
            format!(
            "UPDATE background_jobs SET retry_deadline_ms={retry}, not_before_ms={not_before}, \
             target_node_id={target_node}, failed_attempts={failed} WHERE id='{target}'"),
        );
        // Its interest.
        let waiter_state = *rng.pick(&[
            "pending",
            "pending",
            "pending",
            "pending",
            "pending",
            "cancelled",
        ]);
        let consumer = *rng.pick(&["analysis", "analysis", "playback_fragment"]);
        let deadline = rng
            .pick(&[
                "NULL".to_owned(),
                "NULL".to_owned(),
                "NULL".to_owned(),
                format!("{}", now - 1),
                format!("{}", now + 1_000),
            ])
            .clone();
        exec(
            &db,
            &mut state,
            format!(
            "UPDATE background_job_waiters SET state='{waiter_state}', consumer_kind='{consumer}', \
             deadline_ms={deadline} WHERE job_id='{target}'"),
        );
        // Other readers of the source: none, live, expired, or expired with custody.
        for (slot, other) in others.iter().enumerate() {
            if rng.chance(30) {
                exec(&db, &mut state, format!(
                    "UPDATE background_job_waiters SET consumer_kind='playback_fragment', deadline_ms=NULL WHERE job_id='{other}'"));
            }
            match *rng.pick(&["none", "none", "live", "expired", "custody"]) {
                "live" => exec(&db, &mut state, format!(
                    "INSERT INTO background_job_reservations(resource_key,slot,job_id,fence,expires_at_ms) \
                     VALUES('source_io',{},'{other}',1,{})", slot + 10, now + 60_000)),
                "expired" => exec(&db, &mut state, format!(
                    "INSERT INTO background_job_reservations(resource_key,slot,job_id,fence,expires_at_ms) \
                     VALUES('source_io',{},'{other}',1,{})", slot + 10, now - 1)),
                "custody" => {
                    exec(&db, &mut state, format!(
                        "INSERT INTO background_job_reservations(resource_key,slot,job_id,fence,expires_at_ms) \
                         VALUES('source_io',{},'{other}',1,{})", slot + 10, now - 1));
                    exec(&db, &mut state, format!(
                        "INSERT INTO background_source_custody(custody_id,owner_key,owner_json,resource_keys_json,owner_kind,\
                         work_id,fence,phase,started_at_ms,updated_at_ms,lease_expires_at_ms) VALUES('custody-{slot}',\
                         'owner-{slot}','{{}}','[\"source_io\"]','job','{other}',1,'active',{},{},{})",
                        now - 5_000, now - 5_000, now - 1));
                }
                _ => {}
            }
        }
        // A foreground owner.
        match *rng.pick(&[
            "none",
            "none",
            "startup",
            "idle",
            "startup-expired",
            "idle-expired",
        ]) {
            "startup" => exec(
                &db,
                &mut state,
                foreground("startup", "{}", now + 60_000, now),
            ),
            "idle" => exec(
                &db,
                &mut state,
                foreground("source_idle", "{\"source_quiescent\":1}", now + 60_000, now),
            ),
            "startup-expired" => exec(&db, &mut state, foreground("startup", "{}", now - 1, now)),
            "idle-expired" => exec(
                &db,
                &mut state,
                foreground("source_idle", "{\"source_quiescent\":1}", now - 1, now),
            ),
            _ => {}
        }
        if rng.chance(4) {
            exec(&db, &mut state, "INSERT INTO settings(key,value) VALUES('internal.cluster_job_owner_removed.node-a','1')".into());
        }
        let revision: i64 = db
            .query_row(
                "SELECT revision FROM background_jobs WHERE id=?1",
                [&target],
                |row| row.get(0),
            )
            .expect("revision");
        let stale = rng.chance(5);
        let body = claim_body(&target, kind, revision + i64::from(stale), now);
        let admitted = statement_admits(&db, &body);
        assert!(admitted <= 1, "case {case}: {admitted} rows\n{state}");
        match verdict(&db, &body) {
            Some(verdict) => {
                let admit = verdict["block"].is_null();
                assert_eq!(
                    admit,
                    admitted == 1,
                    "case {case}: verdict {verdict} vs statement {admitted}\nbody {body}\n{state}"
                );
                *tally
                    .entry(verdict["block"].as_str().unwrap_or("admit").to_owned())
                    .or_default() += 1;
            }
            None => {
                assert_eq!(
                    admitted, 0,
                    "case {case}: no verdict yet the statement admits\n{state}"
                );
                *tally.entry("no_verdict".into()).or_default() += 1;
            }
        }
        let current = claim_body(&target, kind, revision, now);
        assert_eq!(
            listed(&db, &target, kind, now),
            statement_admits(&db, &current) == 1,
            "case {case}: admissible listing disagrees with the statement\n{state}"
        );
        db.execute_batch("ROLLBACK TO case_state; RELEASE case_state")
            .expect("case rollback");
    }
    eprintln!("verdict tally over {cases} cases: {tally:?}");
    for outcome in [
        "admit",
        "state",
        "kind_demand",
        "foreground_window",
        "source_holders",
        "maintenance_reader",
    ] {
        assert!(
            tally.get(outcome).copied().unwrap_or(0) > 0,
            "no {outcome} case: {tally:?}"
        );
    }
}

fn foreground(phase: &str, projection: &str, lease_expires_at_ms: i64, now: i64) -> String {
    format!(
        "INSERT INTO foreground_source_owners(owner_id,identity_json,resource_keys_json,fence,phase,projection_json,\
         started_at_ms,updated_at_ms,lease_expires_at_ms) VALUES('viewer','{{}}','[\"source_io\"]',1,'{phase}',\
         '{projection}',{},{},{lease_expires_at_ms})",
        now - 1_000,
        now - 1_000
    )
}
