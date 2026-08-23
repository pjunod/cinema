//! Replicated storage identities, immutable generations, and reader pins.

use std::path::{Component, Path};

use async_trait::async_trait;
use hiqlite::macros::params;
use hiqlite::Row;

use super::hiqlite::{database_error, validate_sql, HiqliteAuthStore};
use super::SharedCacheStore;
use crate::cluster::coordination::Lease;
use crate::domain::{
    CacheConsumerKind, CacheConsumerPin, CacheStorageMember, SharedCacheGeneration,
};
use crate::error::StoreError;

const ADD_STORAGE_ID: &str =
    "ALTER TABLE transcode_cache_locations ADD COLUMN storage_id TEXT NOT NULL DEFAULT ''";
const ADD_GENERATION_ID: &str =
    "ALTER TABLE transcode_cache_locations ADD COLUMN generation_id TEXT NOT NULL DEFAULT ''";
const BACKFILL_LOCATION_IDENTITY: &str = "UPDATE transcode_cache_locations
    SET storage_id = 'node:' || node_id || ':cache', generation_id = relative_dir
    WHERE storage_id = ''";
const STORAGE_GENERATION_INDEX: &str =
    "CREATE UNIQUE INDEX IF NOT EXISTS transcode_cache_storage_generation
    ON transcode_cache_locations(recipe_hash, storage_id, generation_id)
    WHERE storage_id <> '' AND generation_id <> ''";
const STORAGE_LRU_INDEX: &str = "CREATE INDEX IF NOT EXISTS transcode_cache_storage_lru
    ON transcode_cache_locations(storage_id, complete, last_used_at)";
const LEGACY_LOCATION_IDENTITY_TRIGGER: &str =
    "CREATE TRIGGER IF NOT EXISTS transcode_cache_location_identity_ai
    AFTER INSERT ON transcode_cache_locations
    WHEN new.storage_id = '' AND new.generation_id = '' BEGIN
        UPDATE transcode_cache_locations
           SET storage_id = 'node:' || new.node_id || ':cache',
               generation_id = new.relative_dir
         WHERE recipe_hash = new.recipe_hash
           AND node_id = new.node_id
           AND storage_class = new.storage_class;
    END";
const LEGACY_LOCATION_GENERATION_TRIGGER: &str =
    "CREATE TRIGGER IF NOT EXISTS transcode_cache_location_identity_au
    AFTER UPDATE OF relative_dir ON transcode_cache_locations
    WHEN new.storage_class = 'local'
     AND new.storage_id = 'node:' || new.node_id || ':cache'
     AND new.generation_id = old.generation_id
     AND new.relative_dir <> old.relative_dir BEGIN
        UPDATE transcode_cache_locations
           SET generation_id = new.relative_dir
         WHERE recipe_hash = new.recipe_hash
           AND node_id = new.node_id
           AND storage_class = new.storage_class;
    END";
const STORAGE_MEMBERS_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS cache_storage_members (
    storage_id          TEXT NOT NULL,
    node_id             TEXT NOT NULL,
    storage_class       TEXT NOT NULL CHECK (storage_class IN ('local', 'shared')),
    verified_at_ms      INTEGER NOT NULL,
    verification_state TEXT NOT NULL CHECK (
        verification_state IN ('verified', 'suspect', 'unverified')),
    PRIMARY KEY (storage_id, node_id)
) STRICT";
const STORAGE_MEMBERS_INDEX: &str = "CREATE INDEX IF NOT EXISTS cache_storage_members_node
    ON cache_storage_members(node_id, verification_state, storage_id)";
const CONSUMER_PINS_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS cache_consumer_pins (
    storage_id       TEXT NOT NULL,
    recipe_hash      TEXT NOT NULL,
    generation_id    TEXT NOT NULL,
    consumer_kind    TEXT NOT NULL CHECK (consumer_kind IN (
        'media_session', 'offline_package', 'offline_download')),
    consumer_id      TEXT NOT NULL,
    consumer_epoch   INTEGER NOT NULL CHECK (consumer_epoch > 0),
    expires_at_ms    INTEGER NOT NULL,
    PRIMARY KEY (
        storage_id, recipe_hash, generation_id, consumer_kind, consumer_id)
) STRICT";
const CONSUMER_PINS_INDEX: &str = "CREATE INDEX IF NOT EXISTS cache_consumer_pins_expiry
    ON cache_consumer_pins(storage_id, expires_at_ms)";

pub(super) fn migration_statements() -> Vec<(String, hiqlite::Params)> {
    [
        ADD_STORAGE_ID,
        ADD_GENERATION_ID,
        BACKFILL_LOCATION_IDENTITY,
        STORAGE_GENERATION_INDEX,
        STORAGE_LRU_INDEX,
        LEGACY_LOCATION_IDENTITY_TRIGGER,
        LEGACY_LOCATION_GENERATION_TRIGGER,
        STORAGE_MEMBERS_SCHEMA,
        STORAGE_MEMBERS_INDEX,
        CONSUMER_PINS_SCHEMA,
        CONSUMER_PINS_INDEX,
    ]
    .into_iter()
    .map(|sql| (sql.to_owned(), params!()))
    .collect()
}

pub(super) async fn install_schema(client: &hiqlite::Client) -> Result<(), StoreError> {
    // Fresh bootstrap creates these two columns in DURABLE_SCHEMA. Keep the
    // ALTER statements only for the v10 -> v11 migration: bootstrap is
    // intentionally repeatable in validation and must not add them twice.
    let statements = migration_statements()
        .into_iter()
        .skip(2)
        .collect::<Vec<_>>();
    for (sql, _) in &statements {
        validate_sql(sql)?;
    }
    client
        .txn(statements)
        .await
        .map_err(database_error)?
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .map_err(database_error)?;
    Ok(())
}

const SHARED_COLS: &str = "l.recipe_hash, r.file_id, l.storage_id, l.generation_id, \
    l.relative_dir, l.bytes, l.manifest_digest, l.last_used_at, l.complete";

#[async_trait]
impl SharedCacheStore for HiqliteAuthStore {
    async fn put_cache_storage_member(
        &self,
        member: &CacheStorageMember,
    ) -> Result<(), StoreError> {
        validate_member(member)?;
        let sql = "INSERT INTO cache_storage_members
                (storage_id, node_id, storage_class, verified_at_ms, verification_state)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT(storage_id, node_id) DO UPDATE SET
                storage_class = excluded.storage_class,
                verified_at_ms = excluded.verified_at_ms,
                verification_state = excluded.verification_state";
        validate_sql(sql)?;
        self.client()
            .execute(
                sql,
                params!(
                    &member.storage_id,
                    &member.node_id,
                    &member.storage_class,
                    member.verified_at_ms,
                    &member.verification_state
                ),
            )
            .await?;
        Ok(())
    }

    async fn cache_storage_member(
        &self,
        storage_id: &str,
        node_id: &str,
    ) -> Result<Option<CacheStorageMember>, StoreError> {
        validate_id("storage id", storage_id)?;
        validate_id("node id", node_id)?;
        let sql = "SELECT storage_id, node_id, storage_class, verified_at_ms,
                        verification_state
                   FROM cache_storage_members
                  WHERE storage_id = $1 AND node_id = $2";
        validate_sql(sql)?;
        Ok(self
            .client()
            .query_consistent_map::<MemberRow, _>(sql, params!(storage_id, node_id))
            .await?
            .into_iter()
            .next()
            .map(Into::into))
    }

    async fn mark_cache_storage_suspect(
        &self,
        storage_id: &str,
        node_id: &str,
        observed_at_ms: i64,
    ) -> Result<bool, StoreError> {
        validate_id("storage id", storage_id)?;
        validate_id("node id", node_id)?;
        let sql = "UPDATE cache_storage_members
                    SET verification_state = 'suspect', verified_at_ms = $1
                  WHERE storage_id = $2 AND node_id = $3";
        validate_sql(sql)?;
        Ok(self
            .client()
            .execute(sql, params!(observed_at_ms, storage_id, node_id))
            .await?
            == 1)
    }

    async fn shared_cache_hit(
        &self,
        recipe_hash: &str,
        storage_id: &str,
    ) -> Result<Option<SharedCacheGeneration>, StoreError> {
        validate_id("recipe hash", recipe_hash)?;
        validate_id("storage id", storage_id)?;
        let sql = format!(
            "SELECT {SHARED_COLS}
               FROM transcode_cache_locations l
               JOIN transcode_cache_recipes r ON r.recipe_hash = l.recipe_hash
              WHERE l.recipe_hash = $1 AND l.storage_id = $2
                AND l.storage_class = 'shared' AND l.complete = 1"
        );
        validate_sql(&sql)?;
        self.client()
            .query_consistent_map::<SharedRow, _>(sql, params!(recipe_hash, storage_id))
            .await?
            .into_iter()
            .next()
            .map(TryInto::try_into)
            .transpose()
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
        let sql = "UPDATE transcode_cache_locations
                    SET last_used_at = MAX(last_used_at, $1),
                        last_seen_at = MAX(last_seen_at, $1)
                  WHERE recipe_hash = $2 AND storage_id = $3 AND generation_id = $4
                    AND storage_class = 'shared' AND complete = 1";
        validate_sql(sql)?;
        Ok(self
            .client()
            .execute(sql, params!(now_ms, recipe_hash, storage_id, generation_id))
            .await?
            == 1)
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
        let statements = vec![
            (
                "INSERT INTO transcode_cache_recipes
                    (recipe_hash, file_id, recipe_version, created_at)
                 VALUES ($1, $2, $3, $4)
                 ON CONFLICT(recipe_hash) DO NOTHING"
                    .to_owned(),
                params!(recipe_hash, file_id, recipe_version, now_ms),
            ),
            (
                "INSERT INTO transcode_cache_locations
                    (recipe_hash, node_id, storage_class, relative_dir, bytes, complete,
                     manifest_digest, scrub_object_index, last_used_at, last_seen_at,
                     storage_id, generation_id)
                 SELECT $1, $2, 'shared', $3, 0, 0, NULL, 0, $4, $4, $2, $5
                  WHERE EXISTS (
                    SELECT 1 FROM cache_storage_members
                     WHERE storage_id = $2 AND storage_class = 'shared'
                       AND verification_state = 'verified')
                 ON CONFLICT(recipe_hash, node_id, storage_class) DO NOTHING"
                    .to_owned(),
                params!(recipe_hash, storage_id, relative_dir, now_ms, generation_id),
            ),
            (
                "DELETE FROM transcode_cache_recipes
                  WHERE recipe_hash = $1
                    AND NOT EXISTS (
                        SELECT 1 FROM transcode_cache_locations
                         WHERE recipe_hash = $1)"
                    .to_owned(),
                params!(recipe_hash),
            ),
        ];
        for (sql, _) in &statements {
            validate_sql(sql)?;
        }
        let results = self
            .client()
            .txn(statements)
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        Ok(results.get(1).copied().unwrap_or_default() == 1)
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
        let statements = vec![
            (
                "UPDATE transcode_cache_locations
                    SET complete = 1, bytes = $1, manifest_digest = $2, last_seen_at = $3
                  WHERE recipe_hash = $4 AND storage_id = $5 AND generation_id = $6
                    AND storage_class = 'shared' AND complete = 0"
                    .to_owned(),
                params!(
                    bytes,
                    manifest_digest,
                    now_ms,
                    recipe_hash,
                    storage_id,
                    generation_id
                ),
            ),
            (
                "INSERT INTO cache_consumer_pins
                    (storage_id, recipe_hash, generation_id, consumer_kind,
                     consumer_id, consumer_epoch, expires_at_ms)
                 SELECT location.storage_id, location.recipe_hash, location.generation_id,
                        'offline_package', package.id, 1,
                        CASE WHEN package.expires_at > 9223372036854775
                             THEN 9223372036854775807
                             ELSE package.expires_at * 1000 END
                   FROM transcode_cache_locations location
                   JOIN offline_packages package
                     ON package.recipe_hash = location.recipe_hash
                  WHERE location.recipe_hash = $1 AND location.storage_id = $2
                    AND location.generation_id = $3 AND location.storage_class = 'shared'
                    AND location.complete = 1 AND package.state = 'ready'
                 ON CONFLICT(
                    storage_id, recipe_hash, generation_id, consumer_kind, consumer_id)
                 DO UPDATE SET expires_at_ms = MAX(
                    cache_consumer_pins.expires_at_ms, excluded.expires_at_ms)"
                    .to_owned(),
                params!(recipe_hash, storage_id, generation_id),
            ),
            (
                "INSERT INTO cache_consumer_pins
                    (storage_id, recipe_hash, generation_id, consumer_kind,
                     consumer_id, consumer_epoch, expires_at_ms)
                 SELECT location.storage_id, location.recipe_hash, location.generation_id,
                        'offline_download', lease.token_hash, 1,
                        CASE WHEN lease.expires_at > 9223372036854775
                             THEN 9223372036854775807
                             ELSE lease.expires_at * 1000 END
                   FROM transcode_cache_locations location
                   JOIN offline_packages package
                     ON package.recipe_hash = location.recipe_hash
                   JOIN offline_package_leases lease ON lease.package_id = package.id
                  WHERE location.recipe_hash = $1 AND location.storage_id = $2
                    AND location.generation_id = $3 AND location.storage_class = 'shared'
                    AND location.complete = 1 AND package.state = 'ready'
                 ON CONFLICT(
                    storage_id, recipe_hash, generation_id, consumer_kind, consumer_id)
                 DO UPDATE SET expires_at_ms = MAX(
                    cache_consumer_pins.expires_at_ms, excluded.expires_at_ms)"
                    .to_owned(),
                params!(recipe_hash, storage_id, generation_id),
            ),
        ];
        for (sql, _) in &statements {
            validate_sql(sql)?;
        }
        let results = self
            .client()
            .txn(statements)
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        Ok(results.first().copied().unwrap_or_default() == 1)
    }

    async fn abandon_shared_cache_entry(
        &self,
        recipe_hash: &str,
        storage_id: &str,
        generation_id: &str,
        relative_dir: &str,
    ) -> Result<bool, StoreError> {
        validate_id("recipe hash", recipe_hash)?;
        validate_id("storage id", storage_id)?;
        validate_id("generation id", generation_id)?;
        validate_generation(recipe_hash, storage_id, generation_id, relative_dir)?;
        let sql = "UPDATE transcode_cache_locations SET complete = 2
                  WHERE recipe_hash = $1 AND storage_id = $2 AND generation_id = $3
                    AND relative_dir = $4 AND storage_class = 'shared'
                    AND complete IN (0, 2)";
        validate_sql(sql)?;
        Ok(self
            .client()
            .execute(
                sql,
                params!(recipe_hash, storage_id, generation_id, relative_dir),
            )
            .await?
            == 1)
    }

    async fn finalize_abandoned_shared_cache_entry(
        &self,
        recipe_hash: &str,
        storage_id: &str,
        generation_id: &str,
        relative_dir: &str,
    ) -> Result<bool, StoreError> {
        validate_id("recipe hash", recipe_hash)?;
        validate_id("storage id", storage_id)?;
        validate_id("generation id", generation_id)?;
        validate_generation(recipe_hash, storage_id, generation_id, relative_dir)?;
        let statements = vec![
            (
                "DELETE FROM transcode_cache_locations
                  WHERE recipe_hash = $1 AND storage_id = $2 AND generation_id = $3
                    AND relative_dir = $4 AND storage_class = 'shared' AND complete = 2"
                    .to_owned(),
                params!(recipe_hash, storage_id, generation_id, relative_dir),
            ),
            (
                "DELETE FROM transcode_cache_recipes
                  WHERE recipe_hash = $1
                    AND NOT EXISTS (
                        SELECT 1 FROM transcode_cache_locations WHERE recipe_hash = $1)"
                    .to_owned(),
                params!(recipe_hash),
            ),
        ];
        for (sql, _) in &statements {
            validate_sql(sql)?;
        }
        let results = self
            .client()
            .txn(statements)
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        Ok(results.first().copied().unwrap_or_default() == 1)
    }

    async fn stale_shared_cache_claims(
        &self,
        storage_id: &str,
        before_ms: i64,
        limit: i64,
    ) -> Result<Vec<SharedCacheGeneration>, StoreError> {
        validate_id("storage id", storage_id)?;
        if !(1..=1_000).contains(&limit) {
            return Err(StoreError::Task(
                "stale shared cache claim limit must be between 1 and 1000".to_owned(),
            ));
        }
        let sql = format!(
            "SELECT {SHARED_COLS}
               FROM transcode_cache_locations l
               JOIN transcode_cache_recipes r ON r.recipe_hash = l.recipe_hash
              WHERE l.storage_id = $1 AND l.storage_class = 'shared'
                AND (l.complete = 2 OR (l.complete = 0 AND l.last_seen_at <= $2))
              ORDER BY l.last_seen_at, l.recipe_hash, l.generation_id
              LIMIT $3"
        );
        validate_sql(&sql)?;
        self.client()
            .query_consistent_map::<SharedRow, _>(sql, params!(storage_id, before_ms, limit))
            .await?
            .into_iter()
            .map(TryInto::try_into)
            .collect()
    }

    async fn acquire_cache_consumer_pin(
        &self,
        pin: &CacheConsumerPin,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        validate_pin(pin, now_ms)?;
        let sql = "INSERT INTO cache_consumer_pins
                (storage_id, recipe_hash, generation_id, consumer_kind,
                 consumer_id, consumer_epoch, expires_at_ms)
             SELECT $1, $2, $3, $4, $5, $6, $7
              WHERE EXISTS (
                SELECT 1 FROM transcode_cache_locations
                 WHERE recipe_hash = $2 AND storage_id = $1
                   AND generation_id = $3 AND storage_class = 'shared'
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
             WHERE excluded.consumer_epoch >= cache_consumer_pins.consumer_epoch";
        validate_sql(sql)?;
        Ok(self
            .client()
            .execute(
                sql,
                params!(
                    &pin.storage_id,
                    &pin.recipe_hash,
                    &pin.generation_id,
                    pin.consumer_kind.as_str(),
                    &pin.consumer_id,
                    pin.consumer_epoch,
                    pin.expires_at_ms
                ),
            )
            .await?
            == 1)
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
        if pins.is_empty() {
            return Ok(0);
        }
        let mut statements = Vec::with_capacity(pins.len());
        for pin in pins {
            validate_pin(pin, now_ms)?;
            statements.push((
                "UPDATE cache_consumer_pins
                    SET expires_at_ms = MAX(expires_at_ms, $1)
                  WHERE storage_id = $2 AND recipe_hash = $3
                    AND generation_id = $4 AND consumer_kind = $5
                    AND consumer_id = $6 AND consumer_epoch = $7
                    AND expires_at_ms > $8
                    AND EXISTS (
                        SELECT 1 FROM transcode_cache_locations
                         WHERE recipe_hash = $3 AND storage_id = $2
                           AND generation_id = $4 AND storage_class = 'shared'
                           AND complete = 1)"
                    .to_owned(),
                params!(
                    pin.expires_at_ms,
                    &pin.storage_id,
                    &pin.recipe_hash,
                    &pin.generation_id,
                    pin.consumer_kind.as_str(),
                    &pin.consumer_id,
                    pin.consumer_epoch,
                    now_ms
                ),
            ));
        }
        for (sql, _) in &statements {
            validate_sql(sql)?;
        }
        let changed = self
            .client()
            .txn(statements)
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?
            .into_iter()
            .sum::<usize>();
        Ok(changed)
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
        let sql = "DELETE FROM cache_consumer_pins
                  WHERE storage_id = $1 AND recipe_hash = $2 AND generation_id = $3
                    AND consumer_kind = $4 AND consumer_id = $5 AND consumer_epoch = $6";
        validate_sql(sql)?;
        Ok(self
            .client()
            .execute(
                sql,
                params!(
                    storage_id,
                    recipe_hash,
                    generation_id,
                    consumer_kind.as_str(),
                    consumer_id,
                    consumer_epoch
                ),
            )
            .await?
            == 1)
    }

    async fn prune_expired_cache_consumer_pins(
        &self,
        storage_id: &str,
        now_ms: i64,
        limit: i64,
    ) -> Result<usize, StoreError> {
        validate_id("storage id", storage_id)?;
        if now_ms < 0 || !(1..=4_096).contains(&limit) {
            return Err(StoreError::Task(
                "expired cache pin prune inputs are invalid".to_owned(),
            ));
        }
        let sql = "DELETE FROM cache_consumer_pins WHERE rowid IN (
                   SELECT rowid FROM cache_consumer_pins
                    WHERE storage_id = $1 AND expires_at_ms <= $2
                    ORDER BY expires_at_ms, recipe_hash, generation_id,
                             consumer_kind, consumer_id
                    LIMIT $3)";
        validate_sql(sql)?;
        self.client()
            .execute(sql, params!(storage_id, now_ms, limit))
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
        let sql = format!(
            "SELECT {SHARED_COLS}
               FROM transcode_cache_locations l
              JOIN transcode_cache_recipes r ON r.recipe_hash = l.recipe_hash
              WHERE l.storage_id = $1 AND l.storage_class = 'shared'
                AND (l.complete = 3 OR (
                    l.complete = 1 AND NOT EXISTS (
                        SELECT 1 FROM cache_consumer_pins p
                         WHERE p.storage_id = l.storage_id
                           AND p.recipe_hash = l.recipe_hash
                           AND p.generation_id = l.generation_id
                           AND p.expires_at_ms > $2)))
              ORDER BY CASE WHEN l.complete = 3 THEN 0 ELSE 1 END,
                       l.last_used_at, l.recipe_hash, l.generation_id
              LIMIT $3"
        );
        validate_sql(&sql)?;
        self.client()
            .query_consistent_map::<SharedRow, _>(sql, params!(storage_id, now_ms, limit))
            .await?
            .into_iter()
            .map(TryInto::try_into)
            .collect()
    }

    async fn retire_shared_cache_generation(
        &self,
        generation: &SharedCacheGeneration,
        now_ms: i64,
        lease: &Lease,
    ) -> Result<Option<Lease>, StoreError> {
        validate_generation(
            &generation.recipe_hash,
            &generation.storage_id,
            &generation.generation_id,
            &generation.relative_dir,
        )?;
        let expected_resource = format!("shared-cache-gc:{}", generation.storage_id);
        if lease.resource != expected_resource || lease.expires_at_unix_ms <= now_ms {
            return Ok(None);
        }
        let successor = lease.publication_successor()?;
        let fence = i64::try_from(lease.fence)
            .map_err(|error| StoreError::Task(format!("GC lease fence is invalid: {error}")))?;
        let revision = i64::try_from(lease.revision)
            .map_err(|error| StoreError::Task(format!("GC lease revision is invalid: {error}")))?;
        let statements = vec![
            (
                "UPDATE transcode_cache_locations SET complete = 3
                  WHERE recipe_hash = $1 AND storage_id = $2 AND generation_id = $3
                    AND storage_class = 'shared' AND complete IN (1, 3)
                    AND relative_dir = $4
                    AND (manifest_digest = $5
                         OR (manifest_digest IS NULL AND $5 IS NULL))
                    AND EXISTS (
                        SELECT 1 FROM job_leases
                         WHERE resource = $6 AND owner_node_id = $7
                           AND fence = $8 AND revision = $9
                           AND expires_at_ms = $10 AND expires_at_ms > $11)
                    AND NOT EXISTS (
                        SELECT 1 FROM cache_consumer_pins p
                         WHERE p.storage_id = $2 AND p.recipe_hash = $1
                           AND p.generation_id = $3 AND p.expires_at_ms > $11)"
                    .to_owned(),
                params!(
                    &generation.recipe_hash,
                    &generation.storage_id,
                    &generation.generation_id,
                    &generation.relative_dir,
                    &generation.manifest_digest,
                    &lease.resource,
                    &lease.owner_node_id,
                    fence,
                    revision,
                    lease.expires_at_unix_ms,
                    now_ms
                ),
            ),
            (
                "UPDATE job_leases
                    SET revision = $1, expires_at_ms = $2, updated_at_ms = $3
                  WHERE resource = $4 AND owner_node_id = $5 AND fence = $6
                    AND revision = $7 AND expires_at_ms = $8
                    AND expires_at_ms > $9
                    AND EXISTS (
                        SELECT 1 FROM transcode_cache_locations l
                         WHERE l.recipe_hash = $10 AND l.storage_id = $11
                           AND l.generation_id = $12 AND l.storage_class = 'shared'
                           AND l.complete = 3 AND l.relative_dir = $13
                           AND (l.manifest_digest = $14
                                OR (l.manifest_digest IS NULL AND $14 IS NULL))
                           AND NOT EXISTS (
                               SELECT 1 FROM cache_consumer_pins p
                                WHERE p.storage_id = l.storage_id
                                  AND p.recipe_hash = l.recipe_hash
                                  AND p.generation_id = l.generation_id
                                  AND p.expires_at_ms > $9))"
                    .to_owned(),
                params!(
                    i64::try_from(successor.revision).map_err(|error| StoreError::Task(
                        format!("GC successor revision is invalid: {error}")
                    ))?,
                    successor.expires_at_unix_ms,
                    now_ms,
                    &lease.resource,
                    &lease.owner_node_id,
                    fence,
                    revision,
                    lease.expires_at_unix_ms,
                    now_ms,
                    &generation.recipe_hash,
                    &generation.storage_id,
                    &generation.generation_id,
                    &generation.relative_dir,
                    &generation.manifest_digest
                ),
            ),
        ];
        for (sql, _) in &statements {
            validate_sql(sql)?;
        }
        let results = self
            .client()
            .txn(statements)
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        Ok((results.as_slice() == [1, 1]).then_some(successor))
    }

    async fn finalize_retired_shared_cache_generation(
        &self,
        generation: &SharedCacheGeneration,
        now_ms: i64,
        lease: &Lease,
    ) -> Result<Option<Lease>, StoreError> {
        validate_generation(
            &generation.recipe_hash,
            &generation.storage_id,
            &generation.generation_id,
            &generation.relative_dir,
        )?;
        let expected_resource = format!("shared-cache-gc:{}", generation.storage_id);
        if lease.resource != expected_resource || lease.expires_at_unix_ms <= now_ms {
            return Ok(None);
        }
        let successor = lease.publication_successor()?;
        let fence = i64::try_from(lease.fence)
            .map_err(|error| StoreError::Task(format!("GC lease fence is invalid: {error}")))?;
        let revision = i64::try_from(lease.revision)
            .map_err(|error| StoreError::Task(format!("GC lease revision is invalid: {error}")))?;
        let successor_revision = i64::try_from(successor.revision).map_err(|error| {
            StoreError::Task(format!("GC successor revision is invalid: {error}"))
        })?;
        let statements = vec![
            (
                "UPDATE job_leases
                    SET revision = $1, expires_at_ms = $2, updated_at_ms = $3
                  WHERE resource = $4 AND owner_node_id = $5 AND fence = $6
                    AND revision = $7 AND expires_at_ms = $8
                    AND expires_at_ms > $9
                    AND EXISTS (
                        SELECT 1 FROM transcode_cache_locations
                         WHERE recipe_hash = $10 AND storage_id = $11
                           AND generation_id = $12 AND storage_class = 'shared'
                           AND complete = 3 AND relative_dir = $13
                           AND (manifest_digest = $14
                             OR (manifest_digest IS NULL AND $14 IS NULL)))"
                    .to_owned(),
                params!(
                    successor_revision,
                    successor.expires_at_unix_ms,
                    now_ms,
                    &lease.resource,
                    &lease.owner_node_id,
                    fence,
                    revision,
                    lease.expires_at_unix_ms,
                    now_ms,
                    &generation.recipe_hash,
                    &generation.storage_id,
                    &generation.generation_id,
                    &generation.relative_dir,
                    &generation.manifest_digest
                ),
            ),
            (
                "DELETE FROM transcode_cache_locations
                  WHERE recipe_hash = $1 AND storage_id = $2 AND generation_id = $3
                    AND storage_class = 'shared' AND complete = 3
                    AND relative_dir = $4
                    AND (manifest_digest = $5
                         OR (manifest_digest IS NULL AND $5 IS NULL))
                    AND EXISTS (
                        SELECT 1 FROM job_leases
                         WHERE resource = $6 AND owner_node_id = $7
                           AND fence = $8 AND revision = $9
                           AND expires_at_ms = $10 AND expires_at_ms > $11)"
                    .to_owned(),
                params!(
                    &generation.recipe_hash,
                    &generation.storage_id,
                    &generation.generation_id,
                    &generation.relative_dir,
                    &generation.manifest_digest,
                    &lease.resource,
                    &lease.owner_node_id,
                    fence,
                    successor_revision,
                    successor.expires_at_unix_ms,
                    now_ms
                ),
            ),
            (
                "DELETE FROM cache_consumer_pins
                  WHERE storage_id = $1 AND recipe_hash = $2
                    AND generation_id = $3
                    AND NOT EXISTS (
                        SELECT 1 FROM transcode_cache_locations
                         WHERE storage_id = $1 AND recipe_hash = $2 AND generation_id = $3)
                    AND EXISTS (
                        SELECT 1 FROM job_leases
                         WHERE resource = $4 AND owner_node_id = $5
                           AND fence = $6 AND revision = $7
                           AND expires_at_ms = $8 AND expires_at_ms > $9)"
                    .to_owned(),
                params!(
                    &generation.storage_id,
                    &generation.recipe_hash,
                    &generation.generation_id,
                    &lease.resource,
                    &lease.owner_node_id,
                    fence,
                    successor_revision,
                    successor.expires_at_unix_ms,
                    now_ms
                ),
            ),
            (
                "DELETE FROM transcode_cache_recipes
                  WHERE recipe_hash = $1
                    AND NOT EXISTS (
                        SELECT 1 FROM transcode_cache_locations WHERE recipe_hash = $1)
                    AND EXISTS (
                        SELECT 1 FROM job_leases
                         WHERE resource = $2 AND owner_node_id = $3
                           AND fence = $4 AND revision = $5
                           AND expires_at_ms = $6 AND expires_at_ms > $7)"
                    .to_owned(),
                params!(
                    &generation.recipe_hash,
                    &lease.resource,
                    &lease.owner_node_id,
                    fence,
                    successor_revision,
                    successor.expires_at_unix_ms,
                    now_ms
                ),
            ),
        ];
        for (sql, _) in &statements {
            validate_sql(sql)?;
        }
        let results = self
            .client()
            .txn(statements)
            .await?
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        Ok((results.first() == Some(&1) && results.get(1) == Some(&1)).then_some(successor))
    }
}

struct MemberRow {
    storage_id: String,
    node_id: String,
    storage_class: String,
    verified_at_ms: i64,
    verification_state: String,
}

impl From<&mut Row<'_>> for MemberRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            storage_id: row.get("storage_id"),
            node_id: row.get("node_id"),
            storage_class: row.get("storage_class"),
            verified_at_ms: row.get("verified_at_ms"),
            verification_state: row.get("verification_state"),
        }
    }
}

impl From<MemberRow> for CacheStorageMember {
    fn from(row: MemberRow) -> Self {
        Self {
            storage_id: row.storage_id,
            node_id: row.node_id,
            storage_class: row.storage_class,
            verified_at_ms: row.verified_at_ms,
            verification_state: row.verification_state,
        }
    }
}

struct SharedRow {
    recipe_hash: String,
    file_id: i64,
    storage_id: String,
    generation_id: String,
    relative_dir: String,
    bytes: i64,
    manifest_digest: Option<String>,
    last_used_at: i64,
    complete: i64,
}

impl From<&mut Row<'_>> for SharedRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            recipe_hash: row.get("recipe_hash"),
            file_id: row.get("file_id"),
            storage_id: row.get("storage_id"),
            generation_id: row.get("generation_id"),
            relative_dir: row.get("relative_dir"),
            bytes: row.get("bytes"),
            manifest_digest: row.get("manifest_digest"),
            last_used_at: row.get("last_used_at"),
            complete: row.get("complete"),
        }
    }
}

impl TryFrom<SharedRow> for SharedCacheGeneration {
    type Error = StoreError;

    fn try_from(row: SharedRow) -> Result<Self, Self::Error> {
        validate_generation(
            &row.recipe_hash,
            &row.storage_id,
            &row.generation_id,
            &row.relative_dir,
        )?;
        Ok(Self {
            recipe_hash: row.recipe_hash,
            file_id: row.file_id,
            storage_id: row.storage_id,
            generation_id: row.generation_id,
            relative_dir: row.relative_dir,
            bytes: row.bytes,
            manifest_digest: row.manifest_digest,
            last_used_at: row.last_used_at,
            cleanup_pending: row.complete == 3,
        })
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
