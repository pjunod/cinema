//! Replicated lease-fenced job publications.
//!
//! Every one-statement mutation carries the exact lease predicate in that
//! statement. Reconciliation prepends the same predicate to its existing Raft
//! transaction guard, so a stale owner cannot publish even after finishing
//! expensive work.

use async_trait::async_trait;
use hiqlite::macros::params;
use hiqlite::{Param, Row};

use super::hiqlite::{database_error, HiqliteAuthStore};
use crate::cluster::coordination::{unix_ms, Lease};
use crate::domain::{
    sort_title_for, ArtworkAttempt, BookMetadataPatch, Item, ItemKind, MetadataPatch, NewItem,
    ProbeResult,
};
use crate::error::StoreError;
use crate::store::{
    ArtworkRepairFence, FencedPublicationStore, ReconcileOutcome, RootFingerprintStatus,
};

const ATOMIC_PUBLICATION_TTL_MS: i64 = 90_000;

pub(super) fn atomic_renewal_statement(
    lease: &Lease,
    replacement: &Lease,
    execution_time_ms: i64,
) -> Result<(String, hiqlite::Params), StoreError> {
    if replacement.resource != lease.resource
        || replacement.owner_node_id != lease.owner_node_id
        || replacement.fence != lease.fence
        || replacement.revision != lease.revision.saturating_add(1)
        || replacement.expires_at_unix_ms <= lease.expires_at_unix_ms
    {
        return Err(StoreError::Task(
            "invalid atomic publication lease replacement".to_owned(),
        ));
    }
    Ok((
        "UPDATE job_leases
            SET revision = $1, expires_at_ms = $2, updated_at_ms = $3
          WHERE resource = $4 AND owner_node_id = $5
            AND fence = $6 AND revision = $7 AND expires_at_ms = $8
            AND expires_at_ms > $9
          RETURNING resource, owner_node_id, fence, revision, expires_at_ms"
            .to_owned(),
        params!(
            lease_i64("replacement revision", replacement.revision)?,
            replacement.expires_at_unix_ms,
            replacement
                .expires_at_unix_ms
                .saturating_sub(ATOMIC_PUBLICATION_TTL_MS),
            lease.resource.as_str(),
            lease.owner_node_id.as_str(),
            lease_i64("fence", lease.fence)?,
            lease_i64("revision", lease.revision)?,
            lease.expires_at_unix_ms,
            execution_time_ms
        ),
    ))
}

fn bind_atomic_authority(params: &mut hiqlite::Params, lease: &Lease) -> Result<(), StoreError> {
    let expected = params!(
        lease.resource.as_str(),
        lease.owner_node_id.as_str(),
        lease_i64("fence", lease.fence)?,
        lease_i64("revision", lease.revision)?,
        lease.expires_at_unix_ms
    );
    let mut matches = 0_usize;
    let mut index = 0_usize;
    while index + expected.len() <= params.len() {
        if params[index..index + expected.len()] == expected {
            for (offset, column) in [
                "resource",
                "owner_node_id",
                "fence",
                "revision",
                "expires_at_ms",
            ]
            .into_iter()
            .enumerate()
            {
                params[index + offset] = Param::StmtOutputNamed(0, column.into());
            }
            matches += 1;
            index += expected.len();
        } else {
            index += 1;
        }
    }
    if matches == 0 {
        return Err(StoreError::Task(
            "atomic publication statement has no exact lease authority predicate".to_owned(),
        ));
    }
    Ok(())
}

fn publication_row_id() -> i64 {
    let bytes = uuid::Uuid::new_v4().into_bytes();
    let mut prefix = [0_u8; 8];
    prefix.copy_from_slice(&bytes[..8]);
    (u64::from_be_bytes(prefix) & i64::MAX as u64).max(1) as i64
}

struct FingerprintRow {
    fingerprint: String,
}

struct IdRow {
    id: i64,
}

impl From<&mut Row<'_>> for IdRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self { id: row.get("id") }
    }
}

struct CountRow {
    count: i64,
}

impl From<&mut Row<'_>> for CountRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            count: row.get("count"),
        }
    }
}

impl From<&mut Row<'_>> for FingerprintRow {
    fn from(row: &mut Row<'_>) -> Self {
        Self {
            fingerprint: row.get("fingerprint"),
        }
    }
}

impl HiqliteAuthStore {
    pub(super) async fn atomic_publication(
        &self,
        lease: &Lease,
        replacement: &Lease,
        mut statements: Vec<(String, hiqlite::Params)>,
    ) -> Result<Vec<usize>, StoreError> {
        if statements.is_empty() {
            return Err(StoreError::Task(
                "atomic publication requires at least one mutation".to_owned(),
            ));
        }
        // Lease expiries use Unix milliseconds. HiqliteAuthStore's injectable
        // clock deliberately uses Unix seconds for ordinary record metadata,
        // so it must not be reused for this authority boundary.
        let renewal = atomic_renewal_statement(lease, replacement, unix_ms()?)?;
        for (_, params) in &mut statements {
            // Renewal is statement zero. Each mutation gets its authority
            // values from that statement's returned row, not caller input.
            // If the exact predecessor is stale or expired, renewal returns
            // no row and Hiqlite rolls the transaction back when this output
            // cannot be bound.
            bind_atomic_authority(params, lease)?;
        }
        statements.insert(0, renewal);
        let transaction = self.client().txn(statements).await;
        let results = match transaction {
            Ok(results) => results,
            Err(error)
                if error
                    .to_string()
                    .contains("StmtIndex(0) does not have observable row output") =>
            {
                return Err(fence_rejected(lease));
            }
            Err(error) => return Err(error),
        }
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .map_err(database_error)?;
        debug_assert_eq!(results.first().copied(), Some(1));
        Ok(results.into_iter().skip(1).collect())
    }
}

#[async_trait]
impl FencedPublicationStore for HiqliteAuthStore {
    async fn put_setting_fenced(
        &self,
        key: &str,
        value: &str,
        lease: &Lease,
        replacement: &Lease,
    ) -> Result<(), StoreError> {
        let now = self.now()?;
        self.atomic_publication(
            lease,
            replacement,
            vec![(
                "INSERT INTO settings (key, value, updated_at)
                 SELECT $1, $2, $3 WHERE EXISTS (
                   SELECT 1 FROM job_leases WHERE resource = $4 AND owner_node_id = $5
                     AND fence = $6 AND revision = $7 AND expires_at_ms = $8)
                 ON CONFLICT(key) DO UPDATE SET
                   value = excluded.value, updated_at = excluded.updated_at"
                    .to_owned(),
                params!(
                    key,
                    value,
                    now,
                    lease.resource.as_str(),
                    lease.owner_node_id.as_str(),
                    lease_i64("fence", lease.fence)?,
                    lease_i64("revision", lease.revision)?,
                    lease.expires_at_unix_ms
                ),
            )],
        )
        .await?;
        Ok(())
    }

    async fn put_setting_if_absent_fenced(
        &self,
        key: &str,
        value: &str,
        lease: &Lease,
        replacement: &Lease,
    ) -> Result<bool, StoreError> {
        let now = self.now()?;
        let results = self
            .atomic_publication(
                lease,
                replacement,
                vec![(
                    "INSERT INTO settings (key, value, updated_at)
                 SELECT $1, $2, $3 WHERE EXISTS (
                   SELECT 1 FROM job_leases WHERE resource = $4 AND owner_node_id = $5
                     AND fence = $6 AND revision = $7 AND expires_at_ms = $8)
                 ON CONFLICT(key) DO NOTHING"
                        .to_owned(),
                    params!(
                        key,
                        value,
                        now,
                        lease.resource.as_str(),
                        lease.owner_node_id.as_str(),
                        lease_i64("fence", lease.fence)?,
                        lease_i64("revision", lease.revision)?,
                        lease.expires_at_unix_ms
                    ),
                )],
            )
            .await?;
        Ok(results.first().copied() == Some(1))
    }

    async fn put_setting_if_absent_if_artwork_repair_current_fenced(
        &self,
        key: &str,
        value: &str,
        expected_item_id: i64,
        repair_fence: &ArtworkRepairFence,
        lease: &Lease,
        replacement: &Lease,
    ) -> Result<bool, StoreError> {
        let now = self.now()?;
        let results = self
            .atomic_publication(
                lease,
                replacement,
                vec![(
                    "INSERT INTO settings (key, value, updated_at)
                 SELECT $1, $2, $3 WHERE $4 = $5 AND EXISTS (
                   SELECT 1 FROM cluster_artwork_repairs
                   WHERE item_id = $5 AND owner_node_id = $6 AND leader_term = $7
                     AND generation = $8) AND EXISTS (
                   SELECT 1 FROM job_leases WHERE resource = $9 AND owner_node_id = $10
                     AND fence = $11 AND revision = $12 AND expires_at_ms = $13)
                 ON CONFLICT(key) DO NOTHING"
                        .to_owned(),
                    params!(
                        key,
                        value,
                        now,
                        expected_item_id,
                        repair_fence.item_id,
                        repair_fence.owner_node_id.as_str(),
                        repair_fence.leader_term,
                        repair_fence.generation,
                        lease.resource.as_str(),
                        lease.owner_node_id.as_str(),
                        lease_i64("fence", lease.fence)?,
                        lease_i64("revision", lease.revision)?,
                        lease.expires_at_unix_ms
                    ),
                )],
            )
            .await?;
        Ok(results.first().copied() == Some(1))
    }

    async fn mark_library_scanned_fenced(
        &self,
        id: i64,
        refreshed: bool,
        lease: &Lease,
        replacement: &Lease,
    ) -> Result<(), StoreError> {
        let now = self.now()?;
        self.atomic_publication(
            lease,
            replacement,
            vec![(
                "UPDATE libraries SET last_scan_at = $1,
                   last_refresh_at = CASE WHEN $2 THEN $1 ELSE last_refresh_at END
                 WHERE id = $3 AND EXISTS (
                   SELECT 1 FROM job_leases WHERE resource = $4 AND owner_node_id = $5
                     AND fence = $6 AND revision = $7 AND expires_at_ms = $8)"
                    .to_owned(),
                params!(
                    now,
                    refreshed,
                    id,
                    lease.resource.as_str(),
                    lease.owner_node_id.as_str(),
                    lease_i64("fence", lease.fence)?,
                    lease_i64("revision", lease.revision)?,
                    lease.expires_at_unix_ms
                ),
            )],
        )
        .await?;
        Ok(())
    }

    async fn insert_item_fenced(
        &self,
        item: &NewItem,
        lease: &Lease,
        replacement: &Lease,
    ) -> Result<i64, StoreError> {
        let sort_title = if item.kind == ItemKind::Folder {
            item.title.to_lowercase()
        } else {
            sort_title_for(&item.title)
        };
        let now = self.now()?;
        let id = publication_row_id();
        let results = self
            .atomic_publication(
                lease,
                replacement,
                vec![(
                    "INSERT INTO items
                   (id, library_id, kind, parent_id, title, sort_title, year,
                    season_number, episode_number, added_at, updated_at)
                 SELECT $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $10
                 WHERE EXISTS (
                   SELECT 1 FROM job_leases WHERE resource = $11 AND owner_node_id = $12
                     AND fence = $13 AND revision = $14 AND expires_at_ms = $15)"
                        .to_owned(),
                    params!(
                        id,
                        item.library_id,
                        item.kind.as_str(),
                        item.parent_id,
                        item.title.as_str(),
                        sort_title,
                        item.year,
                        item.season_number,
                        item.episode_number,
                        now,
                        lease.resource.as_str(),
                        lease.owner_node_id.as_str(),
                        lease_i64("fence", lease.fence)?,
                        lease_i64("revision", lease.revision)?,
                        lease.expires_at_unix_ms
                    ),
                )],
            )
            .await?;
        if results.first().copied() == Some(1) {
            Ok(id)
        } else {
            Err(StoreError::Database(
                "atomic fenced item insert changed no row".to_owned(),
            ))
        }
    }

    async fn apply_metadata_fenced(
        &self,
        item_id: i64,
        patch: &MetadataPatch,
        lease: &Lease,
        replacement: &Lease,
    ) -> Result<(), StoreError> {
        let sort_title = patch.title.as_deref().map(sort_title_for);
        let tags = patch
            .tags
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(database_error)?;
        let genres = patch
            .genres
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(database_error)?;
        let artwork_error = match &patch.artwork {
            Some(ArtworkAttempt::Failed(reason)) => Some(reason.as_str()),
            _ => None,
        };
        let now = self.now()?;
        self.atomic_publication(
            lease,
            replacement,
            vec![(
                "UPDATE items SET
                   title = COALESCE($1, title), sort_title = COALESCE($2, sort_title),
                   year = COALESCE($3, year), overview = COALESCE($4, overview),
                   tmdb_id = COALESCE($5, tmdb_id), imdb_id = COALESCE($6, imdb_id),
                   air_date = COALESCE($7, air_date), runtime_ms = COALESCE($8, runtime_ms),
                   poster_path = COALESCE($9, poster_path),
                   backdrop_path = COALESCE($10, backdrop_path),
                   recorded_at = COALESCE($11, recorded_at), tags = COALESCE($12, tags),
                   genres = COALESCE($13, genres),
                   artwork_error = CASE WHEN $14 = 1 THEN $15 ELSE artwork_error END,
                   metadata_at = CASE WHEN $16 = 1 THEN $17 ELSE metadata_at END,
                   artwork_attempted_at = CASE WHEN $14 = 1 THEN $17 ELSE artwork_attempted_at END,
                   updated_at = $17
                 WHERE id = $18 AND EXISTS (
                   SELECT 1 FROM job_leases WHERE resource = $19 AND owner_node_id = $20
                     AND fence = $21 AND revision = $22 AND expires_at_ms = $23)"
                    .to_owned(),
                params!(
                    patch.title.as_deref(),
                    sort_title,
                    patch.year,
                    patch.overview.as_deref(),
                    patch.tmdb_id,
                    patch.imdb_id.as_deref(),
                    patch.air_date.as_deref(),
                    patch.runtime_ms,
                    patch.poster_path.as_deref(),
                    patch.backdrop_path.as_deref(),
                    patch.recorded_at.as_deref(),
                    tags,
                    genres,
                    patch.artwork.is_some(),
                    artwork_error,
                    patch.enriched,
                    now,
                    item_id,
                    lease.resource.as_str(),
                    lease.owner_node_id.as_str(),
                    lease_i64("fence", lease.fence)?,
                    lease_i64("revision", lease.revision)?,
                    lease.expires_at_unix_ms
                ),
            )],
        )
        .await?;
        Ok(())
    }

    async fn apply_metadata_if_artwork_repair_current_fenced(
        &self,
        item_id: i64,
        patch: &MetadataPatch,
        repair_fence: &ArtworkRepairFence,
        lease: &Lease,
        replacement: &Lease,
    ) -> Result<bool, StoreError> {
        let sort_title = patch.title.as_deref().map(sort_title_for);
        let tags = patch
            .tags
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(database_error)?;
        let genres = patch
            .genres
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(database_error)?;
        let artwork_error = match &patch.artwork {
            Some(ArtworkAttempt::Failed(reason)) => Some(reason.as_str()),
            _ => None,
        };
        let now = self.now()?;
        let results = self
            .atomic_publication(
                lease,
                replacement,
                vec![(
                    "UPDATE items SET
                   title = COALESCE($1, title), sort_title = COALESCE($2, sort_title),
                   year = COALESCE($3, year), overview = COALESCE($4, overview),
                   tmdb_id = COALESCE($5, tmdb_id), imdb_id = COALESCE($6, imdb_id),
                   air_date = COALESCE($7, air_date), runtime_ms = COALESCE($8, runtime_ms),
                   poster_path = COALESCE($9, poster_path),
                   backdrop_path = COALESCE($10, backdrop_path),
                   recorded_at = COALESCE($11, recorded_at), tags = COALESCE($12, tags),
                   genres = COALESCE($13, genres),
                   artwork_error = CASE WHEN $14 = 1 THEN $15 ELSE artwork_error END,
                   metadata_at = CASE WHEN $16 = 1 THEN $17 ELSE metadata_at END,
                   artwork_attempted_at = CASE WHEN $14 = 1 THEN $17 ELSE artwork_attempted_at END,
                   updated_at = $17
                 WHERE id = $18 AND $19 = $18 AND EXISTS (
                   SELECT 1 FROM cluster_artwork_repairs
                   WHERE item_id = $19 AND owner_node_id = $20 AND leader_term = $21
                     AND generation = $22) AND EXISTS (
                   SELECT 1 FROM job_leases WHERE resource = $23 AND owner_node_id = $24
                     AND fence = $25 AND revision = $26 AND expires_at_ms = $27)"
                        .to_owned(),
                    params!(
                        patch.title.as_deref(),
                        sort_title,
                        patch.year,
                        patch.overview.as_deref(),
                        patch.tmdb_id,
                        patch.imdb_id.as_deref(),
                        patch.air_date.as_deref(),
                        patch.runtime_ms,
                        patch.poster_path.as_deref(),
                        patch.backdrop_path.as_deref(),
                        patch.recorded_at.as_deref(),
                        tags,
                        genres,
                        patch.artwork.is_some(),
                        artwork_error,
                        patch.enriched,
                        now,
                        item_id,
                        repair_fence.item_id,
                        repair_fence.owner_node_id.as_str(),
                        repair_fence.leader_term,
                        repair_fence.generation,
                        lease.resource.as_str(),
                        lease.owner_node_id.as_str(),
                        lease_i64("fence", lease.fence)?,
                        lease_i64("revision", lease.revision)?,
                        lease.expires_at_unix_ms
                    ),
                )],
            )
            .await?;
        Ok(results.first().copied() == Some(1))
    }

    async fn apply_book_metadata_fenced(
        &self,
        item_id: i64,
        patch: &BookMetadataPatch,
        lease: &Lease,
        replacement: &Lease,
    ) -> Result<(), StoreError> {
        let sort_title = patch.title.as_deref().map(sort_title_for);
        let source = patch.source.as_str();
        let now = self.now()?;
        self.atomic_publication(
            lease,
            replacement,
            vec![(
                "UPDATE items SET
                   title = CASE WHEN ($1 = 'curator' OR book_metadata_source IS NULL
                                            OR book_metadata_source = 'epub')
                                  THEN COALESCE($2, title) ELSE title END,
                   sort_title = CASE WHEN ($1 = 'curator' OR book_metadata_source IS NULL
                                                 OR book_metadata_source = 'epub')
                                       THEN COALESCE($3, sort_title) ELSE sort_title END,
                   author = CASE WHEN ($1 = 'curator' OR book_metadata_source IS NULL
                                             OR book_metadata_source = 'epub')
                                  THEN COALESCE($4, author) ELSE author END,
                   book_work_id = CASE WHEN ($1 = 'curator' OR book_metadata_source IS NULL
                                                   OR book_metadata_source = 'epub')
                                      THEN COALESCE($5, book_work_id) ELSE book_work_id END,
                   book_edition_id = CASE WHEN ($1 = 'curator' OR book_metadata_source IS NULL
                                                      OR book_metadata_source = 'epub')
                                         THEN COALESCE($6, book_edition_id)
                                         ELSE book_edition_id END,
                   poster_path = CASE WHEN ($1 = 'curator' OR book_metadata_source IS NULL
                                                  OR book_metadata_source = 'epub')
                                      THEN COALESCE($7, poster_path) ELSE poster_path END,
                   book_metadata_source = CASE
                     WHEN ($1 = 'curator' OR book_metadata_source IS NULL
                                           OR book_metadata_source = 'epub') THEN $1
                     ELSE book_metadata_source END,
                   updated_at = CASE
                     WHEN ($1 = 'curator' OR book_metadata_source IS NULL
                                           OR book_metadata_source = 'epub') THEN $8
                     ELSE updated_at END
                 WHERE id = $9 AND kind IN ('book','audiobook') AND EXISTS (
                   SELECT 1 FROM job_leases WHERE resource = $10 AND owner_node_id = $11
                     AND fence = $12 AND revision = $13 AND expires_at_ms = $14)"
                    .to_owned(),
                params!(
                    source,
                    patch.title.as_deref(),
                    sort_title,
                    patch.author.as_deref(),
                    patch.work_id.as_deref(),
                    patch.edition_id.as_deref(),
                    patch.poster_path.as_deref(),
                    now,
                    item_id,
                    lease.resource.as_str(),
                    lease.owner_node_id.as_str(),
                    lease_i64("fence", lease.fence)?,
                    lease_i64("revision", lease.revision)?,
                    lease.expires_at_unix_ms
                ),
            )],
        )
        .await?;
        Ok(())
    }

    async fn apply_book_metadata_if_current_fenced(
        &self,
        expected: &Item,
        patch: &BookMetadataPatch,
        repair_fence: Option<&ArtworkRepairFence>,
        lease: &Lease,
        replacement: &Lease,
    ) -> Result<bool, StoreError> {
        let sort_title = patch.title.as_deref().map(sort_title_for);
        let source = patch.source.as_str();
        let now = self.now()?;
        let (origin_key, origin_value) = patch
            .required_origin
            .as_ref()
            .map_or((None, None), |(key, value)| {
                (Some(key.as_str()), Some(value.as_str()))
            });
        let statement = if let Some(repair_fence) = repair_fence {
            (
                "UPDATE items SET
                   title = COALESCE($1, title), sort_title = COALESCE($2, sort_title),
                   author = COALESCE($3, author), book_work_id = COALESCE($4, book_work_id),
                   book_edition_id = COALESCE($5, book_edition_id),
                   poster_path = COALESCE($6, poster_path), book_metadata_source = $7,
                   updated_at = $8
                 WHERE id = $9 AND kind IN ('book', 'audiobook')
                   AND title = $10 AND author IS $11 AND book_work_id IS $12
                   AND book_metadata_source IS $13 AND book_edition_id IS $14
                   AND poster_path IS $15 AND ($16 IS NULL OR EXISTS (
                     SELECT 1 FROM settings WHERE key = $16 AND value = $17))
                   AND $18 = $9 AND EXISTS (
                     SELECT 1 FROM cluster_artwork_repairs
                     WHERE item_id = $18 AND owner_node_id = $19 AND leader_term = $20
                       AND generation = $21) AND EXISTS (
                     SELECT 1 FROM job_leases WHERE resource = $22 AND owner_node_id = $23
                       AND fence = $24 AND revision = $25 AND expires_at_ms = $26)"
                    .to_owned(),
                params!(
                    patch.title.as_deref(),
                    sort_title,
                    patch.author.as_deref(),
                    patch.work_id.as_deref(),
                    patch.edition_id.as_deref(),
                    patch.poster_path.as_deref(),
                    source,
                    now,
                    expected.id,
                    expected.title.as_str(),
                    expected.author.as_deref(),
                    expected.book_work_id.as_deref(),
                    expected.book_metadata_source.as_deref(),
                    expected.book_edition_id.as_deref(),
                    expected.poster_path.as_deref(),
                    origin_key,
                    origin_value,
                    repair_fence.item_id,
                    repair_fence.owner_node_id.as_str(),
                    repair_fence.leader_term,
                    repair_fence.generation,
                    lease.resource.as_str(),
                    lease.owner_node_id.as_str(),
                    lease_i64("fence", lease.fence)?,
                    lease_i64("revision", lease.revision)?,
                    lease.expires_at_unix_ms
                ),
            )
        } else {
            (
                "UPDATE items SET
                   title = COALESCE($1, title), sort_title = COALESCE($2, sort_title),
                   author = COALESCE($3, author), book_work_id = COALESCE($4, book_work_id),
                   book_edition_id = COALESCE($5, book_edition_id),
                   poster_path = COALESCE($6, poster_path), book_metadata_source = $7,
                   updated_at = $8
                 WHERE id = $9 AND kind IN ('book', 'audiobook')
                   AND title = $10 AND author IS $11 AND book_work_id IS $12
                   AND book_metadata_source IS $13 AND book_edition_id IS $14
                   AND poster_path IS $15 AND ($16 IS NULL OR EXISTS (
                     SELECT 1 FROM settings WHERE key = $16 AND value = $17))
                   AND EXISTS (
                     SELECT 1 FROM job_leases WHERE resource = $18 AND owner_node_id = $19
                       AND fence = $20 AND revision = $21 AND expires_at_ms = $22)"
                    .to_owned(),
                params!(
                    patch.title.as_deref(),
                    sort_title,
                    patch.author.as_deref(),
                    patch.work_id.as_deref(),
                    patch.edition_id.as_deref(),
                    patch.poster_path.as_deref(),
                    source,
                    now,
                    expected.id,
                    expected.title.as_str(),
                    expected.author.as_deref(),
                    expected.book_work_id.as_deref(),
                    expected.book_metadata_source.as_deref(),
                    expected.book_edition_id.as_deref(),
                    expected.poster_path.as_deref(),
                    origin_key,
                    origin_value,
                    lease.resource.as_str(),
                    lease.owner_node_id.as_str(),
                    lease_i64("fence", lease.fence)?,
                    lease_i64("revision", lease.revision)?,
                    lease.expires_at_unix_ms
                ),
            )
        };
        let results = self
            .atomic_publication(lease, replacement, vec![statement])
            .await?;
        Ok(results.first().copied() == Some(1))
    }

    async fn set_nfo_seeded_fenced(
        &self,
        item_id: i64,
        lease: &Lease,
        replacement: &Lease,
    ) -> Result<(), StoreError> {
        let now = self.now()?;
        self.atomic_publication(
            lease,
            replacement,
            vec![(
                "UPDATE items SET nfo_seeded_at = $1 WHERE id = $2 AND EXISTS (
                   SELECT 1 FROM job_leases WHERE resource = $3 AND owner_node_id = $4
                     AND fence = $5 AND revision = $6 AND expires_at_ms = $7)"
                    .to_owned(),
                params!(
                    now,
                    item_id,
                    lease.resource.as_str(),
                    lease.owner_node_id.as_str(),
                    lease_i64("fence", lease.fence)?,
                    lease_i64("revision", lease.revision)?,
                    lease.expires_at_unix_ms
                ),
            )],
        )
        .await?;
        Ok(())
    }

    async fn upsert_file_fenced(
        &self,
        item_id: i64,
        path: &str,
        size: i64,
        mtime: i64,
        probe: &ProbeResult,
        lease: &Lease,
        replacement: &Lease,
    ) -> Result<i64, StoreError> {
        let audio = serde_json::to_string(&probe.audio_streams).map_err(database_error)?;
        let subtitles = serde_json::to_string(&probe.subtitle_streams).map_err(database_error)?;
        let now = self.now()?;
        self.atomic_publication(
            lease,
            replacement,
            vec![(
                "INSERT INTO files
                   (item_id, path, size, mtime, duration_ms, container, video_codec,
                    video_profile, width, height, bit_depth, hdr, bitrate,
                    audio_streams, subtitle_streams, probe_json, hdr_format, scanned_at,
                    dv_profile, dv_level, dv_bl_compat_id, dv_el_present, dv_rpu_present)
                   SELECT $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13,
                        $14, $15, $16, $17, $18, $19, $20, $21, $22, $23 WHERE EXISTS (
                   SELECT 1 FROM job_leases WHERE resource = $24 AND owner_node_id = $25
                     AND fence = $26 AND revision = $27 AND expires_at_ms = $28)
                 ON CONFLICT(path) DO UPDATE SET
                   item_id = excluded.item_id, size = excluded.size, mtime = excluded.mtime,
                   duration_ms = excluded.duration_ms, container = excluded.container,
                   video_codec = excluded.video_codec, video_profile = excluded.video_profile,
                   width = excluded.width, height = excluded.height,
                   bit_depth = excluded.bit_depth, hdr = excluded.hdr,
                   bitrate = excluded.bitrate, audio_streams = excluded.audio_streams,
                   subtitle_streams = excluded.subtitle_streams,
                   probe_json = excluded.probe_json, hdr_format = excluded.hdr_format,
                   dv_profile = excluded.dv_profile, dv_level = excluded.dv_level,
                   dv_bl_compat_id = excluded.dv_bl_compat_id,
                   dv_el_present = excluded.dv_el_present,
                   dv_rpu_present = excluded.dv_rpu_present,
                   scanned_at = excluded.scanned_at"
                    .to_owned(),
                params!(
                    item_id,
                    path,
                    size,
                    mtime,
                    probe.duration_ms,
                    probe.container.as_deref(),
                    probe.video_codec.as_deref(),
                    probe.video_profile.as_deref(),
                    probe.width,
                    probe.height,
                    probe.bit_depth,
                    probe.hdr.as_deref(),
                    probe.bitrate,
                    audio,
                    subtitles,
                    probe.raw_json.as_deref(),
                    probe.hdr_format.as_deref(),
                    now,
                    probe.dolby_vision.profile,
                    probe.dolby_vision.level,
                    probe.dolby_vision.bl_compat_id,
                    probe.dolby_vision.el_present.map(i64::from),
                    probe.dolby_vision.rpu_present.map(i64::from),
                    lease.resource.as_str(),
                    lease.owner_node_id.as_str(),
                    lease_i64("fence", lease.fence)?,
                    lease_i64("revision", lease.revision)?,
                    lease.expires_at_unix_ms
                ),
            )],
        )
        .await?;
        self.client()
            .query_consistent_map::<IdRow, _>("SELECT id FROM files WHERE path = $1", params!(path))
            .await?
            .into_iter()
            .next()
            .map(|row| row.id)
            .ok_or_else(|| {
                StoreError::Database("atomic fenced file upsert changed no row".to_owned())
            })
    }

    async fn ensure_library_root_fingerprint_fenced(
        &self,
        library_id: i64,
        fingerprint: &str,
        allow_establish: bool,
        lease: &Lease,
        replacement: &Lease,
    ) -> Result<RootFingerprintStatus, StoreError> {
        let results = self
            .atomic_publication(
                lease,
                replacement,
                vec![
                    (
                        "INSERT INTO library_roots (library_id, fingerprint)
                 SELECT $1, $2 WHERE $3 AND EXISTS (
                   SELECT 1 FROM job_leases WHERE resource = $4 AND owner_node_id = $5
                     AND fence = $6 AND revision = $7 AND expires_at_ms = $8)
                 ON CONFLICT(library_id) DO NOTHING"
                            .to_owned(),
                        params!(
                            library_id,
                            fingerprint,
                            allow_establish,
                            lease.resource.as_str(),
                            lease.owner_node_id.as_str(),
                            lease_i64("fence", lease.fence)?,
                            lease_i64("revision", lease.revision)?,
                            lease.expires_at_unix_ms
                        ),
                    ),
                    (
                        "UPDATE library_roots SET fingerprint = fingerprint
                 WHERE library_id = $1 AND EXISTS (
                   SELECT 1 FROM job_leases WHERE resource = $2 AND owner_node_id = $3
                     AND fence = $4 AND revision = $5 AND expires_at_ms = $6)"
                            .to_owned(),
                        params!(
                            library_id,
                            lease.resource.as_str(),
                            lease.owner_node_id.as_str(),
                            lease_i64("fence", lease.fence)?,
                            lease_i64("revision", lease.revision)?,
                            lease.expires_at_unix_ms
                        ),
                    ),
                ],
            )
            .await?;
        if results.first().copied() == Some(1) {
            return Ok(RootFingerprintStatus::Established);
        }
        let expected = self
            .client()
            .query_consistent_map::<FingerprintRow, _>(
                "SELECT fingerprint FROM library_roots WHERE library_id = $1",
                params!(library_id),
            )
            .await?
            .into_iter()
            .next()
            .map(|row| row.fingerprint);
        let Some(expected) = expected else {
            return Ok(RootFingerprintStatus::Unestablished);
        };
        if expected == fingerprint {
            Ok(RootFingerprintStatus::Matched)
        } else {
            Ok(RootFingerprintStatus::Mismatch { expected })
        }
    }

    async fn reconcile_library_fenced(
        &self,
        library_id: i64,
        root_fingerprint: &str,
        gone_file_ids: &[i64],
        prune_limit: u64,
        lease: &Lease,
        replacement: &Lease,
    ) -> Result<ReconcileOutcome, StoreError> {
        if let Some(refusal) = super::reconcile_payload_refusal(gone_file_ids, prune_limit) {
            return Ok(refusal);
        }
        let ids = serde_json::to_string(gone_file_ids).map_err(database_error)?;
        let limit = i64::try_from(prune_limit).unwrap_or(i64::MAX);
        let statements = vec![
            (
                "INSERT INTO scan_reconcile_guards (library_id)
                 SELECT $1 WHERE EXISTS (SELECT 1 FROM library_roots
                   WHERE library_id = $1 AND fingerprint = $2)
                 AND (SELECT COUNT(*) FROM files f JOIN items i ON i.id = f.item_id
                   WHERE i.library_id = $1
                     AND f.id IN (SELECT value FROM json_each($3))) <= $4
                AND EXISTS (SELECT 1 FROM job_leases
                   WHERE resource = $5 AND owner_node_id = $6
                     AND fence = $7 AND revision = $8 AND expires_at_ms = $9)
                 ON CONFLICT(library_id) DO NOTHING"
                    .to_owned(),
                params!(
                    library_id,
                    root_fingerprint,
                    ids.as_str(),
                    limit,
                    lease.resource.as_str(),
                    lease.owner_node_id.as_str(),
                    lease_i64("fence", lease.fence)?,
                    lease_i64("revision", lease.revision)?,
                    lease.expires_at_unix_ms
                ),
            ),
            (
                "DELETE FROM files WHERE item_id IN
                   (SELECT id FROM items WHERE library_id = $1)
                 AND id IN (SELECT value FROM json_each($2))
                 AND EXISTS (SELECT 1 FROM scan_reconcile_guards WHERE library_id = $1)
                 AND EXISTS (SELECT 1 FROM job_leases WHERE resource = $3
                   AND owner_node_id = $4 AND fence = $5 AND revision = $6
                   AND expires_at_ms = $7)"
                    .to_owned(),
                params!(
                    library_id,
                    ids.as_str(),
                    lease.resource.as_str(),
                    lease.owner_node_id.as_str(),
                    lease_i64("fence", lease.fence)?,
                    lease_i64("revision", lease.revision)?,
                    lease.expires_at_unix_ms
                ),
            ),
            (
                "DELETE FROM items WHERE library_id = $1
                 AND EXISTS (SELECT 1 FROM scan_reconcile_guards WHERE library_id = $1)
                 AND kind IN ('movie','episode','video','photo','book','audiobook')
                 AND id NOT IN (SELECT item_id FROM files)
                 AND EXISTS (SELECT 1 FROM job_leases WHERE resource = $2
                   AND owner_node_id = $3 AND fence = $4 AND revision = $5
                   AND expires_at_ms = $6)"
                    .to_owned(),
                reconcile_lease_params(library_id, lease)?,
            ),
            (
                "DELETE FROM items WHERE library_id = $1
                 AND EXISTS (SELECT 1 FROM scan_reconcile_guards WHERE library_id = $1)
                 AND kind = 'season' AND id NOT IN (SELECT parent_id FROM items
                   WHERE kind = 'episode' AND parent_id IS NOT NULL)
                 AND EXISTS (SELECT 1 FROM job_leases WHERE resource = $2
                   AND owner_node_id = $3 AND fence = $4 AND revision = $5
                   AND expires_at_ms = $6)"
                    .to_owned(),
                reconcile_lease_params(library_id, lease)?,
            ),
            (
                "DELETE FROM items WHERE library_id = $1
                 AND EXISTS (SELECT 1 FROM scan_reconcile_guards WHERE library_id = $1)
                 AND kind = 'show' AND id NOT IN (SELECT parent_id FROM items
                   WHERE kind = 'season' AND parent_id IS NOT NULL)
                 AND EXISTS (SELECT 1 FROM job_leases WHERE resource = $2
                   AND owner_node_id = $3 AND fence = $4 AND revision = $5
                   AND expires_at_ms = $6)"
                    .to_owned(),
                reconcile_lease_params(library_id, lease)?,
            ),
            (
                "WITH RECURSIVE descendants(root_id, id, kind) AS (
                   SELECT root.id, child.id, child.kind FROM items root
                   LEFT JOIN items child ON child.parent_id = root.id
                   WHERE root.library_id = $1 AND root.kind = 'folder'
                   UNION SELECT descendants.root_id, child.id, child.kind
                   FROM descendants JOIN items child ON child.parent_id = descendants.id
                 ) INSERT INTO scan_reconcile_items (library_id, item_id)
                   SELECT $1, items.id FROM items
                   WHERE items.library_id = $1 AND items.kind = 'folder'
                     AND EXISTS (SELECT 1 FROM scan_reconcile_guards WHERE library_id = $1)
                     AND NOT EXISTS (SELECT 1 FROM descendants
                       WHERE root_id = items.id AND kind != 'folder')
                     AND EXISTS (SELECT 1 FROM job_leases WHERE resource = $2
                       AND owner_node_id = $3 AND fence = $4 AND revision = $5
                       AND expires_at_ms = $6)
                   ON CONFLICT(library_id, item_id) DO NOTHING"
                    .to_owned(),
                reconcile_lease_params(library_id, lease)?,
            ),
            (
                "DELETE FROM items WHERE library_id = $1 AND id IN
                   (SELECT item_id FROM scan_reconcile_items WHERE library_id = $1)
                 AND EXISTS (SELECT 1 FROM job_leases WHERE resource = $2
                   AND owner_node_id = $3 AND fence = $4 AND revision = $5
                   AND expires_at_ms = $6)"
                    .to_owned(),
                reconcile_lease_params(library_id, lease)?,
            ),
            (
                "DELETE FROM scan_reconcile_items WHERE library_id = $1
                 AND EXISTS (SELECT 1 FROM job_leases WHERE resource = $2
                   AND owner_node_id = $3 AND fence = $4 AND revision = $5
                   AND expires_at_ms = $6)"
                    .to_owned(),
                reconcile_lease_params(library_id, lease)?,
            ),
            (
                "DELETE FROM scan_reconcile_guards WHERE library_id = $1
                 AND EXISTS (SELECT 1 FROM job_leases WHERE resource = $2
                   AND owner_node_id = $3 AND fence = $4 AND revision = $5
                   AND expires_at_ms = $6)"
                    .to_owned(),
                reconcile_lease_params(library_id, lease)?,
            ),
        ];
        let results = self
            .atomic_publication(lease, replacement, statements)
            .await?;
        if results.first().copied() != Some(1) {
            let expected = self
                .client()
                .query_consistent_map::<FingerprintRow, _>(
                    "SELECT fingerprint FROM library_roots WHERE library_id = $1",
                    params!(library_id),
                )
                .await?
                .into_iter()
                .next()
                .map(|row| row.fingerprint);
            if expected.as_deref() != Some(root_fingerprint) {
                return Ok(ReconcileOutcome::RefusedRoot {
                    expected: expected.unwrap_or_else(|| "<unregistered>".to_owned()),
                });
            }
            let requested = self
                .client()
                .query_consistent_map::<CountRow, _>(
                    "SELECT COUNT(*) AS count FROM files f JOIN items i ON i.id = f.item_id
                     WHERE i.library_id = $1
                       AND f.id IN (SELECT value FROM json_each($2))",
                    params!(library_id, ids.as_str()),
                )
                .await?
                .into_iter()
                .next()
                .map_or(0, |row| row.count.max(0) as u64);
            if requested > prune_limit {
                return Ok(ReconcileOutcome::RefusedPrune {
                    requested,
                    limit: prune_limit,
                });
            }
            return Err(StoreError::Database(
                "scan reconciliation guard was already held by another operation".to_owned(),
            ));
        }
        Ok(ReconcileOutcome::Applied {
            deleted_files: results[1] as u64,
            pruned_items: [results[2], results[3], results[4], results[6]]
                .into_iter()
                .map(|rows| rows as u64)
                .sum(),
        })
    }

    async fn claim_cache_entry_fenced(
        &self,
        recipe_hash: &str,
        file_id: i64,
        recipe_version: i64,
        node_id: &str,
        relative_dir: &str,
        lease: &Lease,
        replacement: &Lease,
    ) -> Result<bool, StoreError> {
        let now = self.now()?;
        let fence = lease_i64("fence", lease.fence)?;
        let revision = lease_i64("revision", lease.revision)?;
        let results = self
            .atomic_publication(
                lease,
                replacement,
                vec![
                    (
                        "INSERT INTO transcode_cache_recipes
                   (recipe_hash, file_id, recipe_version, created_at)
                 SELECT $1, $2, $3, $4 WHERE EXISTS (
                   SELECT 1 FROM job_leases WHERE resource = $5 AND owner_node_id = $6
                     AND fence = $7 AND revision = $8 AND expires_at_ms = $9)
                 ON CONFLICT(recipe_hash) DO NOTHING"
                            .to_owned(),
                        params!(
                            recipe_hash,
                            file_id,
                            recipe_version,
                            now,
                            lease.resource.as_str(),
                            lease.owner_node_id.as_str(),
                            fence,
                            revision,
                            lease.expires_at_unix_ms
                        ),
                    ),
                    (
                        "INSERT INTO transcode_cache_locations
                   (recipe_hash, node_id, storage_class, relative_dir, bytes, complete,
                    last_used_at, last_seen_at)
                 SELECT $1, $2, 'local', $3, 0, 0, $4, $4 WHERE EXISTS (
                   SELECT 1 FROM job_leases WHERE resource = $5 AND owner_node_id = $6
                     AND fence = $7 AND revision = $8 AND expires_at_ms = $9)
                 ON CONFLICT(recipe_hash, node_id, storage_class) DO UPDATE SET
                   relative_dir = excluded.relative_dir,
                   last_seen_at = MAX(transcode_cache_locations.last_seen_at,
                                      excluded.last_seen_at)
                 WHERE transcode_cache_locations.complete = 0"
                            .to_owned(),
                        params!(
                            recipe_hash,
                            node_id,
                            relative_dir,
                            now,
                            lease.resource.as_str(),
                            lease.owner_node_id.as_str(),
                            fence,
                            revision,
                            lease.expires_at_unix_ms
                        ),
                    ),
                ],
            )
            .await?;
        let claimed = results.get(1).copied().unwrap_or_default();
        Ok(claimed > 0)
    }

    async fn touch_cache_claim_fenced(
        &self,
        recipe_hash: &str,
        node_id: &str,
        lease: &Lease,
        replacement: &Lease,
    ) -> Result<(), StoreError> {
        self.atomic_publication(
            lease,
            replacement,
            vec![(
                "UPDATE transcode_cache_locations SET last_seen_at = MAX(last_seen_at, $1)
                 WHERE recipe_hash = $2 AND node_id = $3 AND complete = 0 AND EXISTS (
                   SELECT 1 FROM job_leases WHERE resource = $4 AND owner_node_id = $5
                     AND fence = $6 AND revision = $7 AND expires_at_ms = $8)"
                    .to_owned(),
                params!(
                    self.now()?,
                    recipe_hash,
                    node_id,
                    lease.resource.as_str(),
                    lease.owner_node_id.as_str(),
                    lease_i64("fence", lease.fence)?,
                    lease_i64("revision", lease.revision)?,
                    lease.expires_at_unix_ms
                ),
            )],
        )
        .await?;
        Ok(())
    }

    async fn complete_cache_entry_fenced(
        &self,
        recipe_hash: &str,
        node_id: &str,
        relative_dir: &str,
        bytes: i64,
        lease: &Lease,
        replacement: &Lease,
    ) -> Result<(), StoreError> {
        let now = self.now()?;
        self.atomic_publication(
            lease,
            replacement,
            vec![(
                "UPDATE transcode_cache_locations
                 SET relative_dir = $1, complete = 1, bytes = $2,
                     last_used_at = MAX(last_used_at, $3),
                     last_seen_at = MAX(last_seen_at, $3)
                 WHERE recipe_hash = $4 AND node_id = $5 AND storage_class = 'local'
                   AND EXISTS (SELECT 1 FROM job_leases
                     WHERE resource = $6 AND owner_node_id = $7
                       AND fence = $8 AND revision = $9 AND expires_at_ms = $10)"
                    .to_owned(),
                params!(
                    relative_dir,
                    bytes,
                    now,
                    recipe_hash,
                    node_id,
                    lease.resource.as_str(),
                    lease.owner_node_id.as_str(),
                    lease_i64("fence", lease.fence)?,
                    lease_i64("revision", lease.revision)?,
                    lease.expires_at_unix_ms
                ),
            )],
        )
        .await?;
        Ok(())
    }

    async fn forget_cache_entry_fenced(
        &self,
        recipe_hash: &str,
        node_id: &str,
        storage_class: &str,
        lease: &Lease,
        replacement: &Lease,
    ) -> Result<(), StoreError> {
        let fence = lease_i64("fence", lease.fence)?;
        let revision = lease_i64("revision", lease.revision)?;
        let results = self
            .atomic_publication(
                lease,
                replacement,
                vec![
                    (
                        "DELETE FROM transcode_cache_locations
                 WHERE recipe_hash = $1 AND node_id = $2 AND storage_class = $3
                   AND EXISTS (SELECT 1 FROM job_leases
                     WHERE resource = $4 AND owner_node_id = $5
                       AND fence = $6 AND revision = $7 AND expires_at_ms = $8)"
                            .to_owned(),
                        params!(
                            recipe_hash,
                            node_id,
                            storage_class,
                            lease.resource.as_str(),
                            lease.owner_node_id.as_str(),
                            fence,
                            revision,
                            lease.expires_at_unix_ms
                        ),
                    ),
                    (
                        "DELETE FROM transcode_cache_recipes WHERE recipe_hash = $1
                     AND NOT EXISTS (SELECT 1 FROM transcode_cache_locations
                                     WHERE recipe_hash = $1)
                     AND EXISTS (SELECT 1 FROM job_leases
                       WHERE resource = $2 AND owner_node_id = $3
                         AND fence = $4 AND revision = $5 AND expires_at_ms = $6)"
                            .to_owned(),
                        params!(
                            recipe_hash,
                            lease.resource.as_str(),
                            lease.owner_node_id.as_str(),
                            fence,
                            revision,
                            lease.expires_at_unix_ms
                        ),
                    ),
                ],
            )
            .await?;
        let _forgotten = results.first().copied().unwrap_or_default();
        Ok(())
    }

    async fn mark_dv_conversion_running_fenced(
        &self,
        file_id: i64,
        bytes_before: i64,
        lease: &Lease,
        replacement: &Lease,
    ) -> Result<bool, StoreError> {
        let results = self
            .atomic_publication(
                lease,
                replacement,
                vec![(
                    "UPDATE dv_conversions
                        SET state = 'running', bytes_before = $2, bytes_after = NULL,
                            error = NULL, finished_at_ms = NULL
                      WHERE file_id = $1 AND state IN ('queued', 'running')
                        AND EXISTS (SELECT 1 FROM job_leases
                          WHERE resource = $3 AND owner_node_id = $4
                            AND fence = $5 AND revision = $6 AND expires_at_ms = $7)"
                        .to_owned(),
                    params!(
                        file_id,
                        bytes_before,
                        lease.resource.as_str(),
                        lease.owner_node_id.as_str(),
                        lease_i64("fence", lease.fence)?,
                        lease_i64("revision", lease.revision)?,
                        lease.expires_at_unix_ms
                    ),
                )],
            )
            .await?;
        Ok(results.first().copied() == Some(1))
    }

    async fn mark_dv_conversion_verified_fenced(
        &self,
        file_id: i64,
        el_type: Option<&str>,
        bytes_after: i64,
        lease: &Lease,
        replacement: &Lease,
    ) -> Result<bool, StoreError> {
        if !matches!(el_type, None | Some("mel" | "fel")) {
            return Err(StoreError::Task(
                "Dolby Vision enhancement-layer type must be mel, fel, or unknown".to_owned(),
            ));
        }
        let results = self
            .atomic_publication(
                lease,
                replacement,
                vec![(
                    "UPDATE dv_conversions
                        SET state = 'verified', el_type = $2, bytes_after = $3, error = NULL
                      WHERE file_id = $1 AND state = 'running'
                        AND EXISTS (SELECT 1 FROM job_leases
                          WHERE resource = $4 AND owner_node_id = $5
                            AND fence = $6 AND revision = $7 AND expires_at_ms = $8)"
                        .to_owned(),
                    params!(
                        file_id,
                        el_type,
                        bytes_after,
                        lease.resource.as_str(),
                        lease.owner_node_id.as_str(),
                        lease_i64("fence", lease.fence)?,
                        lease_i64("revision", lease.revision)?,
                        lease.expires_at_unix_ms
                    ),
                )],
            )
            .await?;
        Ok(results.first().copied() == Some(1))
    }

    async fn mark_dv_conversion_committed_fenced(
        &self,
        file_id: i64,
        original_path: Option<&str>,
        bytes_after: i64,
        finished_at_ms: i64,
        lease: &Lease,
        replacement: &Lease,
    ) -> Result<bool, StoreError> {
        let results = self
            .atomic_publication(
                lease,
                replacement,
                vec![(
                    "UPDATE dv_conversions
                        SET state = 'committed', original_path = $2, bytes_after = $3,
                            error = NULL, finished_at_ms = $4
                      WHERE file_id = $1 AND state = 'verified'
                        AND EXISTS (SELECT 1 FROM job_leases
                          WHERE resource = $5 AND owner_node_id = $6
                            AND fence = $7 AND revision = $8 AND expires_at_ms = $9)"
                        .to_owned(),
                    params!(
                        file_id,
                        original_path,
                        bytes_after,
                        finished_at_ms,
                        lease.resource.as_str(),
                        lease.owner_node_id.as_str(),
                        lease_i64("fence", lease.fence)?,
                        lease_i64("revision", lease.revision)?,
                        lease.expires_at_unix_ms
                    ),
                )],
            )
            .await?;
        Ok(results.first().copied() == Some(1))
    }

    async fn mark_dv_conversion_failed_fenced(
        &self,
        file_id: i64,
        error: &str,
        finished_at_ms: i64,
        lease: &Lease,
        replacement: &Lease,
    ) -> Result<bool, StoreError> {
        let error = error.chars().take(4096).collect::<String>();
        let results = self
            .atomic_publication(
                lease,
                replacement,
                vec![(
                    "UPDATE dv_conversions
                        SET state = 'failed', error = $2, finished_at_ms = $3
                      WHERE file_id = $1 AND state != 'committed'
                        AND EXISTS (SELECT 1 FROM job_leases
                          WHERE resource = $4 AND owner_node_id = $5
                            AND fence = $6 AND revision = $7 AND expires_at_ms = $8)"
                        .to_owned(),
                    params!(
                        file_id,
                        error,
                        finished_at_ms,
                        lease.resource.as_str(),
                        lease.owner_node_id.as_str(),
                        lease_i64("fence", lease.fence)?,
                        lease_i64("revision", lease.revision)?,
                        lease.expires_at_unix_ms
                    ),
                )],
            )
            .await?;
        Ok(results.first().copied() == Some(1))
    }
}

fn lease_i64(label: &str, value: u64) -> Result<i64, StoreError> {
    i64::try_from(value)
        .map_err(|error| StoreError::Database(format!("lease {label} is out of range: {error}")))
}

fn reconcile_lease_params(library_id: i64, lease: &Lease) -> Result<hiqlite::Params, StoreError> {
    Ok(params!(
        library_id,
        lease.resource.as_str(),
        lease.owner_node_id.as_str(),
        lease_i64("fence", lease.fence)?,
        lease_i64("revision", lease.revision)?,
        lease.expires_at_unix_ms
    ))
}

fn fence_rejected(lease: &Lease) -> StoreError {
    StoreError::FenceRejected {
        resource: lease.resource.clone(),
        owner_node_id: lease.owner_node_id.clone(),
        fence: lease.fence,
    }
}
