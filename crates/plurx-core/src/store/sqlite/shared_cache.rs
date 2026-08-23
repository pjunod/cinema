//! Storage-keyed cache generations and distributed reader pins.
//!
//! Local producer-keyed APIs remain in `cache.rs` for rolling binaries.
//! Shared roots use this module so every mutation names immutable storage and
//! generation identities.

use std::path::{Component, Path};

use async_trait::async_trait;
use rusqlite::{params, OptionalExtension};

use super::SqliteStore;
use crate::cluster::coordination::Lease;
use crate::domain::{
    CacheConsumerKind, CacheConsumerPin, CacheStorageMember, SharedCacheGeneration,
};
use crate::error::StoreError;
use crate::store::SharedCacheStore;

const SHARED_COLS: &str = "l.recipe_hash, r.file_id, l.storage_id, l.generation_id, \
    l.relative_dir, l.bytes, l.manifest_digest, l.last_used_at";

fn shared_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SharedCacheGeneration> {
    Ok(SharedCacheGeneration {
        recipe_hash: row.get(0)?,
        file_id: row.get(1)?,
        storage_id: row.get(2)?,
        generation_id: row.get(3)?,
        relative_dir: row.get(4)?,
        bytes: row.get(5)?,
        manifest_digest: row.get(6)?,
        last_used_at: row.get(7)?,
    })
}

fn member_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CacheStorageMember> {
    Ok(CacheStorageMember {
        storage_id: row.get(0)?,
        node_id: row.get(1)?,
        storage_class: row.get(2)?,
        verified_at_ms: row.get(3)?,
        verification_state: row.get(4)?,
    })
}

#[async_trait]
impl SharedCacheStore for SqliteStore {
    async fn put_cache_storage_member(
        &self,
        member: &CacheStorageMember,
    ) -> Result<(), StoreError> {
        validate_member(member)?;
        let member = member.clone();
        self.with_conn(move |conn| {
            conn.execute(
                "INSERT INTO cache_storage_members
                    (storage_id, node_id, storage_class, verified_at_ms, verification_state)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(storage_id, node_id) DO UPDATE SET
                    storage_class = excluded.storage_class,
                    verified_at_ms = excluded.verified_at_ms,
                    verification_state = excluded.verification_state",
                params![
                    member.storage_id,
                    member.node_id,
                    member.storage_class,
                    member.verified_at_ms,
                    member.verification_state
                ],
            )?;
            Ok(())
        })
        .await
    }

    async fn cache_storage_member(
        &self,
        storage_id: &str,
        node_id: &str,
    ) -> Result<Option<CacheStorageMember>, StoreError> {
        validate_id("storage id", storage_id)?;
        validate_id("node id", node_id)?;
        let storage_id = storage_id.to_owned();
        let node_id = node_id.to_owned();
        self.with_conn(move |conn| {
            Ok(conn
                .query_row(
                    "SELECT storage_id, node_id, storage_class, verified_at_ms,
                            verification_state
                       FROM cache_storage_members
                      WHERE storage_id = ?1 AND node_id = ?2",
                    params![storage_id, node_id],
                    member_from_row,
                )
                .optional()?)
        })
        .await
    }

    async fn mark_cache_storage_suspect(
        &self,
        storage_id: &str,
        node_id: &str,
        observed_at_ms: i64,
    ) -> Result<bool, StoreError> {
        validate_id("storage id", storage_id)?;
        validate_id("node id", node_id)?;
        let storage_id = storage_id.to_owned();
        let node_id = node_id.to_owned();
        self.with_conn(move |conn| {
            Ok(conn.execute(
                "UPDATE cache_storage_members
                    SET verification_state = 'suspect', verified_at_ms = ?3
                  WHERE storage_id = ?1 AND node_id = ?2",
                params![storage_id, node_id, observed_at_ms],
            )? == 1)
        })
        .await
    }

    async fn shared_cache_hit(
        &self,
        recipe_hash: &str,
        storage_id: &str,
    ) -> Result<Option<SharedCacheGeneration>, StoreError> {
        validate_id("recipe hash", recipe_hash)?;
        validate_id("storage id", storage_id)?;
        let recipe_hash = recipe_hash.to_owned();
        let storage_id = storage_id.to_owned();
        self.with_conn(move |conn| {
            Ok(conn
                .query_row(
                    &format!(
                        "SELECT {SHARED_COLS}
                           FROM transcode_cache_locations l
                           JOIN transcode_cache_recipes r ON r.recipe_hash = l.recipe_hash
                          WHERE l.recipe_hash = ?1 AND l.storage_id = ?2
                            AND l.storage_class = 'shared' AND l.complete = 1"
                    ),
                    params![recipe_hash, storage_id],
                    shared_from_row,
                )
                .optional()?)
        })
        .await
    }

    async fn touch_shared_cache_entry(
        &self,
        recipe_hash: &str,
        storage_id: &str,
        generation_id: &str,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        validate_id("recipe hash", recipe_hash)?;
        validate_id("storage id", storage_id)?;
        validate_id("generation id", generation_id)?;
        let recipe_hash = recipe_hash.to_owned();
        let storage_id = storage_id.to_owned();
        let generation_id = generation_id.to_owned();
        self.with_conn(move |conn| {
            Ok(conn.execute(
                "UPDATE transcode_cache_locations
                    SET last_used_at = MAX(last_used_at, ?4),
                        last_seen_at = MAX(last_seen_at, ?4)
                  WHERE recipe_hash = ?1 AND storage_id = ?2 AND generation_id = ?3
                    AND storage_class = 'shared' AND complete = 1",
                params![recipe_hash, storage_id, generation_id, now_ms],
            )? == 1)
        })
        .await
    }

    async fn claim_shared_cache_entry(
        &self,
        recipe_hash: &str,
        file_id: i64,
        recipe_version: i64,
        storage_id: &str,
        generation_id: &str,
        relative_dir: &str,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        validate_generation(recipe_hash, storage_id, generation_id, relative_dir)?;
        let recipe_hash = recipe_hash.to_owned();
        let storage_id = storage_id.to_owned();
        let generation_id = generation_id.to_owned();
        let relative_dir = relative_dir.to_owned();
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            tx.execute(
                "INSERT INTO transcode_cache_recipes (recipe_hash, file_id, recipe_version)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(recipe_hash) DO NOTHING",
                params![recipe_hash, file_id, recipe_version],
            )?;
            let changed = tx.execute(
                "INSERT INTO transcode_cache_locations
                    (recipe_hash, node_id, storage_class, relative_dir, bytes, complete,
                     manifest_digest, scrub_object_index, last_used_at, last_seen_at,
                     storage_id, generation_id)
                 SELECT ?1, ?2, 'shared', ?4, 0, 0, NULL, 0, ?5, ?5, ?2, ?3
                  WHERE EXISTS (
                    SELECT 1 FROM cache_storage_members
                     WHERE storage_id = ?2 AND storage_class = 'shared'
                       AND verification_state = 'verified')
                 ON CONFLICT(recipe_hash, node_id, storage_class) DO UPDATE SET
                    relative_dir = excluded.relative_dir,
                    bytes = 0,
                    complete = 0,
                    manifest_digest = NULL,
                    scrub_object_index = 0,
                    last_used_at = excluded.last_used_at,
                    last_seen_at = excluded.last_seen_at,
                    storage_id = excluded.storage_id,
                    generation_id = excluded.generation_id
                 WHERE transcode_cache_locations.complete = 0",
                params![recipe_hash, storage_id, generation_id, relative_dir, now_ms],
            )?;
            if changed == 0 {
                tx.execute(
                    "DELETE FROM transcode_cache_recipes
                      WHERE recipe_hash = ?1
                        AND NOT EXISTS (
                            SELECT 1 FROM transcode_cache_locations
                             WHERE recipe_hash = ?1)",
                    [&recipe_hash],
                )?;
            }
            tx.commit()?;
            Ok(changed == 1)
        })
        .await
    }

    async fn complete_shared_cache_entry(
        &self,
        recipe_hash: &str,
        storage_id: &str,
        generation_id: &str,
        bytes: i64,
        manifest_digest: &str,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        validate_id("recipe hash", recipe_hash)?;
        validate_id("storage id", storage_id)?;
        validate_id("generation id", generation_id)?;
        validate_id("manifest digest", manifest_digest)?;
        if bytes < 0 {
            return Err(StoreError::Task(
                "cache bytes cannot be negative".to_owned(),
            ));
        }
        let recipe_hash = recipe_hash.to_owned();
        let storage_id = storage_id.to_owned();
        let generation_id = generation_id.to_owned();
        let manifest_digest = manifest_digest.to_owned();
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let changed = tx.execute(
                "UPDATE transcode_cache_locations
                    SET complete = 1, bytes = ?4, manifest_digest = ?5,
                        last_seen_at = ?6
                  WHERE recipe_hash = ?1 AND storage_id = ?2
                    AND generation_id = ?3 AND storage_class = 'shared'
                    AND complete = 0",
                params![
                    recipe_hash,
                    storage_id,
                    generation_id,
                    bytes,
                    manifest_digest,
                    now_ms
                ],
            )?;
            if changed == 1 {
                tx.execute(
                    "INSERT INTO cache_consumer_pins
                        (storage_id, recipe_hash, generation_id, consumer_kind,
                         consumer_id, consumer_epoch, expires_at_ms)
                     SELECT ?1, ?2, ?3, 'offline_package', package.id, 1,
                            CASE WHEN package.expires_at > 9223372036854775
                                 THEN 9223372036854775807
                                 ELSE package.expires_at * 1000 END
                       FROM offline_packages package
                      WHERE package.recipe_hash = ?2 AND package.state = 'ready'
                     ON CONFLICT(
                        storage_id, recipe_hash, generation_id, consumer_kind, consumer_id)
                     DO UPDATE SET expires_at_ms = MAX(
                        cache_consumer_pins.expires_at_ms, excluded.expires_at_ms)",
                    params![storage_id, recipe_hash, generation_id],
                )?;
                tx.execute(
                    "INSERT INTO cache_consumer_pins
                        (storage_id, recipe_hash, generation_id, consumer_kind,
                         consumer_id, consumer_epoch, expires_at_ms)
                     SELECT ?1, ?2, ?3, 'offline_download', lease.token_hash, 1,
                            CASE WHEN lease.expires_at > 9223372036854775
                                 THEN 9223372036854775807
                                 ELSE lease.expires_at * 1000 END
                       FROM offline_package_leases lease
                       JOIN offline_packages package ON package.id = lease.package_id
                      WHERE package.recipe_hash = ?2 AND package.state = 'ready'
                     ON CONFLICT(
                        storage_id, recipe_hash, generation_id, consumer_kind, consumer_id)
                     DO UPDATE SET expires_at_ms = MAX(
                        cache_consumer_pins.expires_at_ms, excluded.expires_at_ms)",
                    params![storage_id, recipe_hash, generation_id],
                )?;
            }
            tx.commit()?;
            Ok(changed == 1)
        })
        .await
    }

    async fn acquire_cache_consumer_pin(
        &self,
        pin: &CacheConsumerPin,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        validate_pin(pin, now_ms)?;
        let pin = pin.clone();
        self.with_conn(move |conn| {
            Ok(conn.execute(
                "INSERT INTO cache_consumer_pins
                    (storage_id, recipe_hash, generation_id, consumer_kind,
                     consumer_id, consumer_epoch, expires_at_ms)
                 SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7
                  WHERE EXISTS (
                    SELECT 1 FROM transcode_cache_locations
                     WHERE recipe_hash = ?2 AND storage_id = ?1
                       AND generation_id = ?3 AND storage_class = 'shared'
                       AND complete = 1)
                 ON CONFLICT(
                    storage_id, recipe_hash, generation_id, consumer_kind, consumer_id)
                 DO UPDATE SET
                    consumer_epoch = excluded.consumer_epoch,
                    expires_at_ms = CASE
                        WHEN excluded.consumer_epoch > cache_consumer_pins.consumer_epoch
                            THEN excluded.expires_at_ms
                        WHEN excluded.expires_at_ms > cache_consumer_pins.expires_at_ms
                            THEN excluded.expires_at_ms
                        ELSE cache_consumer_pins.expires_at_ms END
                 WHERE excluded.consumer_epoch >= cache_consumer_pins.consumer_epoch",
                params![
                    pin.storage_id,
                    pin.recipe_hash,
                    pin.generation_id,
                    pin.consumer_kind.as_str(),
                    pin.consumer_id,
                    pin.consumer_epoch,
                    pin.expires_at_ms
                ],
            )? == 1)
        })
        .await
    }

    async fn renew_cache_consumer_pins(
        &self,
        pins: &[CacheConsumerPin],
        now_ms: i64,
    ) -> Result<usize, StoreError> {
        if pins.len() > 256 {
            return Err(StoreError::Task(
                "cache pin renewal batch exceeds 256 entries".to_owned(),
            ));
        }
        for pin in pins {
            validate_pin(pin, now_ms)?;
        }
        let pins = pins.to_vec();
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let mut changed = 0;
            for pin in pins {
                changed += tx.execute(
                    "UPDATE cache_consumer_pins
                        SET expires_at_ms = ?7
                      WHERE storage_id = ?1 AND recipe_hash = ?2
                        AND generation_id = ?3 AND consumer_kind = ?4
                        AND consumer_id = ?5 AND consumer_epoch = ?6
                        AND expires_at_ms > ?8
                        AND EXISTS (
                            SELECT 1 FROM transcode_cache_locations
                             WHERE recipe_hash = ?2 AND storage_id = ?1
                               AND generation_id = ?3 AND storage_class = 'shared'
                               AND complete = 1)",
                    params![
                        pin.storage_id,
                        pin.recipe_hash,
                        pin.generation_id,
                        pin.consumer_kind.as_str(),
                        pin.consumer_id,
                        pin.consumer_epoch,
                        pin.expires_at_ms,
                        now_ms
                    ],
                )?;
            }
            tx.commit()?;
            Ok(changed)
        })
        .await
    }

    async fn release_cache_consumer_pin(
        &self,
        storage_id: &str,
        recipe_hash: &str,
        generation_id: &str,
        consumer_kind: CacheConsumerKind,
        consumer_id: &str,
        consumer_epoch: i64,
    ) -> Result<bool, StoreError> {
        validate_id("storage id", storage_id)?;
        validate_id("recipe hash", recipe_hash)?;
        validate_id("generation id", generation_id)?;
        validate_id("consumer id", consumer_id)?;
        if consumer_epoch <= 0 {
            return Err(StoreError::Task(
                "cache pin consumer epoch must be positive".to_owned(),
            ));
        }
        let storage_id = storage_id.to_owned();
        let recipe_hash = recipe_hash.to_owned();
        let generation_id = generation_id.to_owned();
        let consumer_id = consumer_id.to_owned();
        self.with_conn(move |conn| {
            Ok(conn.execute(
                "DELETE FROM cache_consumer_pins
                  WHERE storage_id = ?1 AND recipe_hash = ?2 AND generation_id = ?3
                    AND consumer_kind = ?4 AND consumer_id = ?5 AND consumer_epoch = ?6",
                params![
                    storage_id,
                    recipe_hash,
                    generation_id,
                    consumer_kind.as_str(),
                    consumer_id,
                    consumer_epoch
                ],
            )? == 1)
        })
        .await
    }

    async fn shared_cache_gc_candidates(
        &self,
        storage_id: &str,
        now_ms: i64,
        limit: i64,
    ) -> Result<Vec<SharedCacheGeneration>, StoreError> {
        validate_id("storage id", storage_id)?;
        if !(1..=1_000).contains(&limit) {
            return Err(StoreError::Task(
                "shared cache GC limit must be between 1 and 1000".to_owned(),
            ));
        }
        let storage_id = storage_id.to_owned();
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {SHARED_COLS}
                   FROM transcode_cache_locations l
                   JOIN transcode_cache_recipes r ON r.recipe_hash = l.recipe_hash
                  WHERE l.storage_id = ?1 AND l.storage_class = 'shared'
                    AND l.complete = 1
                    AND NOT EXISTS (
                        SELECT 1 FROM cache_consumer_pins p
                         WHERE p.storage_id = l.storage_id
                           AND p.recipe_hash = l.recipe_hash
                           AND p.generation_id = l.generation_id
                           AND p.expires_at_ms > ?2)
                  ORDER BY l.last_used_at, l.recipe_hash, l.generation_id
                  LIMIT ?3"
            ))?;
            Ok(stmt
                .query_map(params![storage_id, now_ms, limit], shared_from_row)?
                .collect::<Result<Vec<_>, _>>()?)
        })
        .await
    }

    async fn retire_shared_cache_generation(
        &self,
        generation: &SharedCacheGeneration,
        now_ms: i64,
        lease: &Lease,
    ) -> Result<bool, StoreError> {
        validate_generation(
            &generation.recipe_hash,
            &generation.storage_id,
            &generation.generation_id,
            &generation.relative_dir,
        )?;
        let expected_resource = format!("shared-cache-gc:{}", generation.storage_id);
        if lease.resource != expected_resource || lease.expires_at_unix_ms <= now_ms {
            return Ok(false);
        }
        let fence = i64::try_from(lease.fence)
            .map_err(|error| StoreError::Task(format!("GC lease fence is invalid: {error}")))?;
        let revision = i64::try_from(lease.revision)
            .map_err(|error| StoreError::Task(format!("GC lease revision is invalid: {error}")))?;
        let generation = generation.clone();
        let lease = lease.clone();
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let retired = tx.execute(
                "DELETE FROM transcode_cache_locations
                  WHERE recipe_hash = ?1 AND storage_id = ?2 AND generation_id = ?3
                    AND storage_class = 'shared' AND complete = 1
                    AND relative_dir = ?4
                    AND (manifest_digest = ?5
                         OR (manifest_digest IS NULL AND ?5 IS NULL))
                    AND EXISTS (
                        SELECT 1 FROM job_leases
                         WHERE resource = ?6 AND owner_node_id = ?7
                           AND fence = ?8 AND revision = ?9
                           AND expires_at_ms = ?10 AND expires_at_ms > ?11)
                    AND NOT EXISTS (
                        SELECT 1 FROM cache_consumer_pins p
                         WHERE p.storage_id = ?2 AND p.recipe_hash = ?1
                           AND p.generation_id = ?3 AND p.expires_at_ms > ?11)",
                params![
                    generation.recipe_hash,
                    generation.storage_id,
                    generation.generation_id,
                    generation.relative_dir,
                    generation.manifest_digest,
                    lease.resource,
                    lease.owner_node_id,
                    fence,
                    revision,
                    lease.expires_at_unix_ms,
                    now_ms
                ],
            )?;
            if retired == 1 {
                tx.execute(
                    "DELETE FROM cache_consumer_pins
                      WHERE storage_id = ?1 AND recipe_hash = ?2
                        AND generation_id = ?3 AND expires_at_ms <= ?4",
                    params![
                        generation.storage_id,
                        generation.recipe_hash,
                        generation.generation_id,
                        now_ms
                    ],
                )?;
                tx.execute(
                    "DELETE FROM transcode_cache_recipes
                      WHERE recipe_hash = ?1
                        AND NOT EXISTS (
                            SELECT 1 FROM transcode_cache_locations WHERE recipe_hash = ?1)",
                    params![generation.recipe_hash],
                )?;
            }
            tx.commit()?;
            Ok(retired == 1)
        })
        .await
    }
}

fn validate_member(member: &CacheStorageMember) -> Result<(), StoreError> {
    validate_id("storage id", &member.storage_id)?;
    validate_id("node id", &member.node_id)?;
    if !matches!(member.storage_class.as_str(), "local" | "shared") {
        return Err(StoreError::Task(
            "cache storage class must be local or shared".to_owned(),
        ));
    }
    if !matches!(
        member.verification_state.as_str(),
        "verified" | "suspect" | "unverified"
    ) {
        return Err(StoreError::Task(
            "cache verification state is invalid".to_owned(),
        ));
    }
    Ok(())
}

fn validate_generation(
    recipe_hash: &str,
    storage_id: &str,
    generation_id: &str,
    relative_dir: &str,
) -> Result<(), StoreError> {
    validate_id("recipe hash", recipe_hash)?;
    validate_id("storage id", storage_id)?;
    validate_id("generation id", generation_id)?;
    let path = Path::new(relative_dir);
    if relative_dir.is_empty()
        || relative_dir.len() > 512
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(StoreError::Task(
            "shared cache relative directory is invalid".to_owned(),
        ));
    }
    Ok(())
}

fn validate_pin(pin: &CacheConsumerPin, now_ms: i64) -> Result<(), StoreError> {
    validate_id("storage id", &pin.storage_id)?;
    validate_id("recipe hash", &pin.recipe_hash)?;
    validate_id("generation id", &pin.generation_id)?;
    validate_id("consumer id", &pin.consumer_id)?;
    if pin.consumer_epoch <= 0 || pin.expires_at_ms <= now_ms {
        return Err(StoreError::Task(
            "cache pin epoch and expiry must be live and positive".to_owned(),
        ));
    }
    Ok(())
}

fn validate_id(label: &str, value: &str) -> Result<(), StoreError> {
    if value.is_empty() || value.len() > 256 || value.contains('\0') {
        return Err(StoreError::Task(format!("{label} is invalid")));
    }
    Ok(())
}
