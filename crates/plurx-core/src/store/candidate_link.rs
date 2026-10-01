//! Exact, node-local candidate Link observations. Never submitted to Raft.
use crate::domain::{CandidateLinkBinding, CandidateLinkObservation, CandidateLinkPrior};
use crate::error::StoreError;
use rusqlite::{params, Connection, OptionalExtension};

pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS candidate_link_priors (
 credential TEXT NOT NULL, client TEXT NOT NULL, network TEXT NOT NULL,
 recipe TEXT NOT NULL, binding TEXT NOT NULL, bytes INTEGER NOT NULL,
 body_ms INTEGER NOT NULL, completed_ms INTEGER NOT NULL, negative_ms INTEGER,
 PRIMARY KEY(credential, client, network, recipe)
);";

pub(crate) fn observe(
    conn: &Connection,
    value: &CandidateLinkObservation,
    now: i64,
) -> Result<(), StoreError> {
    if !value.valid_at(now) {
        return Ok(());
    }
    // Both lifetime and total cardinality are bounded across all namespaces.
    conn.execute(
        "DELETE FROM candidate_link_priors WHERE completed_ms < ?1",
        params![now.saturating_sub(7 * 24 * 60 * 60 * 1000)],
    )?;
    let key = &value.binding;
    let bytes = i64::try_from(value.body_bytes)
        .map_err(|error| StoreError::Migration(error.to_string()))?;
    let recipe = hex::encode(key.recipe_digest);
    let encoded =
        serde_json::to_string(key).map_err(|error| StoreError::Migration(error.to_string()))?;
    conn.execute("INSERT INTO candidate_link_priors VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)
      ON CONFLICT(credential,client,network,recipe) DO UPDATE SET
       binding=excluded.binding, bytes=excluded.bytes, body_ms=excluded.body_ms,
       completed_ms=excluded.completed_ms,
       negative_ms=CASE WHEN excluded.negative_ms IS NOT NULL THEN excluded.negative_ms ELSE candidate_link_priors.negative_ms END
      WHERE excluded.completed_ms >= candidate_link_priors.completed_ms AND excluded.binding=candidate_link_priors.binding",
      params![key.credential_generation,key.client_class,key.network_fingerprint,recipe,encoded,
        bytes,value.body_duration_ms,value.completed_at_ms,value.negative.then_some(value.completed_at_ms)])?;
    // Historical negatives cannot keep an unlimited number of recipes resident.
    conn.execute("DELETE FROM candidate_link_priors WHERE credential=?1 AND client=?2 AND network=?3 AND recipe NOT IN
      (SELECT recipe FROM candidate_link_priors WHERE credential=?1 AND client=?2 AND network=?3 ORDER BY completed_ms DESC,recipe LIMIT 64)",
      params![key.credential_generation,key.client_class,key.network_fingerprint])?;
    conn.execute(
        "DELETE FROM candidate_link_priors WHERE rowid NOT IN
      (SELECT rowid FROM candidate_link_priors ORDER BY completed_ms DESC,rowid DESC LIMIT 4096)",
        [],
    )?;
    Ok(())
}

pub(crate) fn get(
    conn: &Connection,
    binding: &CandidateLinkBinding,
) -> Result<Option<CandidateLinkPrior>, StoreError> {
    let raw = conn
        .query_row(
            "SELECT binding,bytes,body_ms,completed_ms,negative_ms FROM candidate_link_priors
      WHERE credential=?1 AND client=?2 AND network=?3 AND recipe=?4",
            params![
                binding.credential_generation,
                binding.client_class,
                binding.network_fingerprint,
                hex::encode(binding.recipe_digest)
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, u32>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                ))
            },
        )
        .optional()?;
    let Some((encoded, body_bytes, body_duration_ms, completed_at_ms, negative_at_ms)) = raw else {
        return Ok(None);
    };
    let saved: CandidateLinkBinding =
        serde_json::from_str(&encoded).map_err(|error| StoreError::Migration(error.to_string()))?;
    if &saved != binding {
        return Ok(None);
    }
    let body_bytes =
        u64::try_from(body_bytes).map_err(|error| StoreError::Migration(error.to_string()))?;
    Ok(Some(CandidateLinkPrior {
        binding: saved,
        body_bytes,
        body_duration_ms,
        completed_at_ms,
        negative_at_ms,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample() -> CandidateLinkObservation {
        CandidateLinkObservation {
            binding: CandidateLinkBinding {
                user_id: 1,
                credential_generation: "a".repeat(64),
                client_class: "web".into(),
                network_fingerprint: "network".into(),
                file_id: 2,
                source_size: 4096,
                source_mtime: 3,
                recipe_digest: [4; 32],
                route: crate::playback::candidate::CandidateRoute::Encode,
            },
            body_bytes: 4096,
            body_duration_ms: 5000,
            completed_at_ms: 100_000,
            negative: false,
        }
    }
    #[test]
    fn a05_bound_storage_keeps_exact_namespace_and_original_negative_time() {
        let conn = Connection::open_in_memory().expect("memory database");
        conn.execute_batch(SCHEMA).expect("schema");
        let mut value = sample();
        observe(&conn, &value, 100_000).expect("positive");
        value.negative = true;
        observe(&conn, &value, 101_000).expect("negative transition");
        let saved = get(&conn, &value.binding).expect("query").expect("prior");
        assert_eq!(saved.negative_at_ms, Some(100_000));
        value.completed_at_ms = 99_999;
        observe(&conn, &value, 101_000).expect("out of order");
        assert_eq!(
            get(&conn, &value.binding)
                .expect("query")
                .expect("prior")
                .completed_at_ms,
            100_000
        );
        let mut mismatch = value.binding.clone();
        mismatch.source_mtime += 1;
        assert!(get(&conn, &mismatch).expect("source mismatch").is_none());
        mismatch = value.binding.clone();
        mismatch.credential_generation = "b".repeat(64);
        assert!(get(&conn, &mismatch)
            .expect("credential mismatch")
            .is_none());
        assert!(!saved.negative_active(100_000 + 7 * 24 * 60 * 60 * 1000 + 1));
        assert!(!saved.negative_active(99_999));
    }
    #[test]
    fn a05_bound_storage_refuses_stale_or_future_completion_without_refresh() {
        let conn = Connection::open_in_memory().expect("memory database");
        conn.execute_batch(SCHEMA).expect("schema");
        let value = sample();
        observe(&conn, &value, 115_001).expect("stale refused");
        observe(&conn, &value, 99_999).expect("future refused");
        assert!(get(&conn, &value.binding).expect("query").is_none());
    }
}
