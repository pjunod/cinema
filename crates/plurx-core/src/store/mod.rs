//! The storage boundary.
//!
//! Everything that must survive a node (and later, replicate across the
//! cluster) goes through the [`Store`] trait family. Phase 0–2:
//! [`SqliteStore`] on local disk. Phase 3 spike / Phase 4: a raft-replicated
//! backend implements these same traits, and single-node mode becomes a
//! 1-voter cluster — same code path. See `docs/ARCHITECTURE.md` §2.
//!
//! The trait is split by domain area purely for readability; consumers hold
//! one `Arc<dyn Store>`. Contract notes for future backends:
//! - Operations are linearizable from the caller's perspective.
//! - A successful write method is durable (on a cluster: quorum-acked).
//!   The HTTP progress coalescer sits above this boundary: an intermediate
//!   active-playback beat may receive a soft acknowledgement while its newest
//!   value is pending, and the response exposes only the durable state.
//! - Implementations are shared via `Arc`, never cloned per-request.

pub mod classification;
pub use classification::ClassificationStore;
mod downloaded_subtitles;
mod dv_conversion;
mod file_grants;
pub use downloaded_subtitles::{
    valid_downloaded_vtt, MAX_DOWNLOADED_SUBTITLES, MAX_DOWNLOADED_SUBTITLE_BYTES,
};
pub use file_grants::{FileGrant, FileGrantStore, NewFileGrant, FILE_GRANTS_SCHEMA};
mod fragindex;
mod fragment_index_cluster;
#[cfg(feature = "hiqlite-store")]
mod hiqlite_classification;
mod renditionplan;
mod sqlite;
mod telemetry;
mod timeline_annotations;

mod publication;
mod scan_identity_repair;
mod sql_source;
pub use scan_identity_repair::{
    plan_identity_repair, IdentityRepairBlocker, IdentityRepairCounts, IdentityRepairFile,
    IdentityRepairFileMove, IdentityRepairItem, IdentityRepairItemMove, IdentityRepairOutcome,
    IdentityRepairPlan, IdentityRepairSnapshot, IdentityRepairWatch, IdentityRepairWatchConflict,
    IdentityRepairWatchCopy, IDENTITY_REPAIR_EPISODES_MAX, IDENTITY_REPAIR_FILES_MAX,
    IDENTITY_REPAIR_PLAN_BYTES_MAX, IDENTITY_REPAIR_SEASONS_MAX, IDENTITY_REPAIR_SHOWS_MAX,
    IDENTITY_REPAIR_SHOWS_MIN, IDENTITY_REPAIR_WATCHES_MAX,
};

#[cfg(feature = "hiqlite-store")]
mod hiqlite;
#[cfg(feature = "hiqlite-store")]
#[doc(hidden)]
pub use hiqlite::validation_time_http_store_operation;
#[cfg(feature = "hiqlite-store")]
mod hiqlite_background_jobs;
#[cfg(feature = "hiqlite-store")]
mod hiqlite_catalog;
#[cfg(feature = "hiqlite-store")]
mod hiqlite_coordination;
#[cfg(feature = "hiqlite-store")]
mod hiqlite_durable;
#[cfg(feature = "hiqlite-store")]
mod hiqlite_dv_conversion;
#[cfg(feature = "hiqlite-store")]
mod hiqlite_dvr;
#[cfg(feature = "hiqlite-store")]
mod hiqlite_fragment_index_cluster;
#[cfg(feature = "hiqlite-store")]
mod hiqlite_import;
#[cfg(feature = "hiqlite-store")]
mod hiqlite_library_channels;
#[cfg(feature = "hiqlite-store")]
mod hiqlite_media;
#[cfg(feature = "hiqlite-store")]
mod hiqlite_pretranscode;
#[cfg(feature = "hiqlite-store")]
mod hiqlite_publication;
#[cfg(feature = "hiqlite-store")]
mod hiqlite_reading;
#[cfg(feature = "hiqlite-store")]
mod hiqlite_sessions;
#[cfg(feature = "hiqlite-store")]
mod hiqlite_shared_cache;
#[cfg(feature = "hiqlite-store")]
mod hiqlite_timeline_annotations;
#[cfg(feature = "hiqlite-store")]
mod watch_fence;

/// The placeholder-order census over every replicated slice above. It is a
/// test module rather than a lint because the rule it enforces is the one
/// `hiqlite::validate_sql` applies at runtime, and the two must not drift.
#[cfg(all(test, feature = "hiqlite-store"))]
mod placeholder_census;

/// K-04 M3: a source census of consistent reads per replicated slice. Not a
/// latency or request-rate measurement; see the module documentation.
#[cfg(all(test, feature = "hiqlite-store"))]
mod consistent_read_census;

pub mod background_jobs;
pub use background_jobs::BackgroundJobStore;
mod background_jobs_delivery;
mod background_jobs_fragment;
pub mod background_jobs_fragment_admission;
mod background_jobs_maintenance;
pub mod background_jobs_pretranscode;
mod background_jobs_publication;
#[cfg(test)]
mod background_jobs_tests;
pub mod replicated;

pub use dv_conversion::{
    DvConversion, DvConversionCandidate, DvConversionMode, DvConversionProgress,
    DvConversionProgressSnapshot, DvConversionQueueBatch, DvConversionState, DvConversionStore,
    DvRecoveryGuard, DvRecoveryGuardSnapshot, DvRecoveryGuardState, DvRecoveryGuardSummary,
    QueueDvConversionOutcome, DV_CONVERSION_LEDGER_READ_MAX, DV_CONVERSION_MODE_DISABLED_REASON,
    DV_CONVERSION_QUEUE_BATCH_MAX, DV_RECOVERY_GUARD_READ_MAX,
};

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

/// Exact replicated exclusion claim required by clustered credential writes.
///
/// Only the membership coordinator can mint one. Store implementations use
/// the opaque value as a transaction predicate so a credential proposal that
/// arrives after cancellation cleanup cannot commit outside its membership
/// exclusion interval. Standalone SQLite passes no claim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CacheAdminMutationClaim(String);

pub const TOKEN_SUMMARY_MAX: usize = 256;

/// Largest device label, in bytes, that a login may persist.
///
/// The label is the one field in `tokens` a caller both chooses and can
/// repeat: every successful login appends a row carrying whatever string the
/// client sent. `TOKEN_SUMMARY_MAX` bounds the device inventory to 256 rows
/// but says nothing about bytes, so without a byte bound one account can mint
/// hundreds of sessions whose labels are each as large as the login body
/// limit allows and turn a single `GET /api/v1/me/devices` into hundreds of
/// MiB of allocation and response — and, on a cluster, replicate each of
/// those labels to every member.
///
/// 256 bytes is the number because a device label is a short human display
/// string: "Paul's iPhone", "Chrome on Windows", "Apple TV (Living Room)", or
/// at the long end a model name derived from a user agent, which runs to
/// roughly 150 bytes. 256 leaves room above every real client while making
/// the worst case arithmetic rather than a hope: 256 rows x 256 bytes is a
/// 64 KiB ceiling on the labels in one inventory response, the same order as
/// the rest of that JSON body. A smaller bound would clip legitimate names in
/// non-Latin scripts, where one character costs three bytes; a larger one buys
/// nothing a client needs and multiplies by 256.
///
/// A label over the bound is **rejected at the write**, not dropped at the
/// read: `POST /api/v1/auth/login` answers `400` and mints no token, so the
/// caller learns immediately rather than discovering later that a device it
/// believes is named is anonymous. Rows an older build already stored are
/// capped by [`bounded_device_label`] where the inventory is projected, so a
/// database that predates this bound still serves a bounded response and
/// still shows every device the user can revoke.
pub const MAX_DEVICE_LABEL_BYTES: usize = 256;

/// Cap a stored label for projection at [`MAX_DEVICE_LABEL_BYTES`].
///
/// Truncation stops at the nearest character boundary at or below the bound,
/// so the result is always valid UTF-8 and never longer than the bound. This
/// runs on read for legacy rows only — new writes are refused above the bound
/// — and it truncates rather than omitting the row, because a device the user
/// cannot see is a device the user cannot revoke.
pub fn bounded_device_label(label: Option<String>) -> Option<String> {
    label.map(|mut label| {
        if label.len() <= MAX_DEVICE_LABEL_BYTES {
            return label;
        }
        let mut end = MAX_DEVICE_LABEL_BYTES;
        while end > 0 && !label.is_char_boundary(end) {
            end -= 1;
        }
        label.truncate(end);
        label
    })
}

/// Privacy-safe login-token metadata for account device management. The full
/// digest remains inside the Store; eight hex characters identify one row
/// only after the Store has proved the prefix is unique for that user.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct TokenSummary {
    pub token_hash_prefix: String,
    pub device: Option<String>,
    pub created_at: i64,
    pub last_seen_at: i64,
}

/// What a login-token lookup found. `Expired` is distinct from `Unknown` so
/// the HTTP layer can tell a client "you were signed out after N idle days"
/// instead of a bare 401; an expired token's activity is never refreshed, so
/// presenting it cannot slide it back to life.
#[derive(Clone, Debug)]
pub enum TokenAuthentication {
    Authenticated(User),
    Expired { idle_days: i64 },
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeleteTokenByPrefixOutcome {
    Deleted,
    NotFound,
    Ambiguous,
    ClaimLost,
}

#[cfg(feature = "hiqlite-store")]
impl CacheAdminMutationClaim {
    pub(crate) fn new(claim_id: String) -> Self {
        Self(claim_id)
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// Keeps the request claim and the route publication fence in one database
/// write. Hiqlite transactions do not expose an application-level rollback
/// after affected-row counts are returned, so this trigger is the common
/// SQLite/Hiqlite atomic boundary for BLOCKED -> finite/ready confirmation.
/// The Dolby Vision configuration record's own fields, as columns on `files`.
///
/// One constant, applied by both backends, so the single-node migration list
/// and the replicated migration chain cannot drift into different shapes.
///
/// Nullable with no default on purpose: a row that predates the backfill and a
/// record that genuinely reported zero have to stay distinguishable, or the
/// fallback to parsing the display label can never know when to stop.
const FILES_DOLBY_VISION_COLUMNS: &[&str] = &[
    "ALTER TABLE files ADD COLUMN dv_profile INTEGER",
    "ALTER TABLE files ADD COLUMN dv_level INTEGER",
    "ALTER TABLE files ADD COLUMN dv_bl_compat_id INTEGER",
    "ALTER TABLE files ADD COLUMN dv_el_present INTEGER",
    "ALTER TABLE files ADD COLUMN dv_rpu_present INTEGER",
];

/// The same five columns as one batch, for the single-node migration list.
///
/// A list rather than a single string because the replicated backend runs one
/// statement per transaction entry and rusqlite refuses a multi-statement
/// `execute`; the batch spelling exists so the append-only SQLite list can
/// keep one element per schema version.
const FILES_DOLBY_VISION_COLUMNS_BATCH: &str = "
ALTER TABLE files ADD COLUMN dv_profile INTEGER;
ALTER TABLE files ADD COLUMN dv_level INTEGER;
ALTER TABLE files ADD COLUMN dv_bl_compat_id INTEGER;
ALTER TABLE files ADD COLUMN dv_el_present INTEGER;
ALTER TABLE files ADD COLUMN dv_rpu_present INTEGER;";

/// The original video's four-character sample-entry label.
///
/// Nullable with no default: rows scanned before this column existed remain
/// explicitly unknown until the bounded stored-probe backfill reaches them.
const FILES_VIDEO_CODEC_TAG_COLUMN: &str = "ALTER TABLE files ADD COLUMN video_codec_tag TEXT;";

/// The selected playable video's field-order token.
///
/// Nullable with no default so the bounded stored-probe backfill can
/// distinguish rows it has not considered from rows it resolved to the
/// explicit `unknown` token.
const FILES_FIELD_ORDER_COLUMN: &str = "ALTER TABLE files ADD COLUMN field_order TEXT;";
/// Source luminance facts used by the CPU tone-map recipe. Nullable values
/// distinguish unknown rows from a completed probe whose `luminance_source`
/// is `none`.
pub(crate) const FILES_LUMINANCE_COLUMNS: &[&str] = &[
    "ALTER TABLE files ADD COLUMN max_cll INTEGER",
    "ALTER TABLE files ADD COLUMN max_fall INTEGER",
    "ALTER TABLE files ADD COLUMN mastering_max_luminance INTEGER",
    "ALTER TABLE files ADD COLUMN luminance_source TEXT CHECK (luminance_source IN ('stream','frame','none'))",
];

const FILES_LUMINANCE_COLUMNS_BATCH: &str = "
ALTER TABLE files ADD COLUMN max_cll INTEGER;
ALTER TABLE files ADD COLUMN max_fall INTEGER;
ALTER TABLE files ADD COLUMN mastering_max_luminance INTEGER;
ALTER TABLE files ADD COLUMN luminance_source TEXT CHECK (luminance_source IN ('stream','frame','none'));";

/// The staged-generation ledger, shared verbatim by both backends.
///
/// One statement, because SQLite's append-only migration list keeps one
/// element per schema version and Hiqlite's `install_schema` keeps one per
/// table. Both spellings have to stay identical or a replicated import will
/// disagree with the node it imported from.
///
/// There is no `state` column and no lifecycle here. A preparation row exists
/// while a staged successor exists; the successor's own `media_sessions` row
/// carries its state, its owner and its lease, exactly like any other. This
/// table answers only the three questions `media_sessions` cannot: which row
/// is staged, which predecessor it was staged against, and when it stops
/// being a candidate.
pub(crate) const MEDIA_SESSION_PREPARATIONS_SCHEMA: &str =
    "CREATE TABLE IF NOT EXISTS media_session_preparations (
        user_id                            INTEGER NOT NULL,
        playback_id                        TEXT NOT NULL
            CHECK (length(playback_id) BETWEEN 1 AND 128),
        staged_incarnation_id              TEXT NOT NULL UNIQUE,
        expected_predecessor_incarnation_id TEXT NOT NULL
            CHECK (length(expected_predecessor_incarnation_id) BETWEEN 1 AND 128),
        deadline_ms                        INTEGER NOT NULL,
        created_at_ms                      INTEGER NOT NULL,
        updated_at_ms                      INTEGER NOT NULL,
        PRIMARY KEY (user_id, playback_id)
    ) STRICT;";

/// The server-owned recovery epoch, on the session row that carries it.
///
/// `media_session_producer_recovery` is keyed by `(user_id, playback_id,
/// recovery_epoch)`, and until this column exists nothing can say what a given
/// session's epoch is — so the budget has a key nobody can present. This is
/// that key, written where a continuation can read it: the predecessor's own
/// row.
///
/// `NOT NULL DEFAULT ''` rather than nullable. Empty means what every row
/// written before this column meant: a session that predates the epoch and
/// therefore has none. It is not a valid epoch — `validated_epoch_key` refuses
/// an empty one — so an empty value cannot accidentally address a budget row.
///
/// One statement, spelled identically for both backends, for the same reason
/// as the two schemas below it.
pub(crate) const MEDIA_SESSION_RECOVERY_EPOCH_SCHEMA: &str = "ALTER TABLE media_sessions
        ADD COLUMN recovery_epoch TEXT NOT NULL DEFAULT '';";

/// Claim fencing and crash-durable one-shot recovery for offline packages.
///
/// `claim_generation` is incremented by the queue claim itself. Recovery is a
/// monotone budget: the terminal primary fault first commits
/// `recovery_pending`, then the current owner freezes its alternate. Re-homing
/// either consumed state moves to `rehome_pending` and clears both node-local
/// recipe references, but can never return the budget to `primary`.
pub(crate) const OFFLINE_CLAIM_GENERATION_SCHEMA: &str = "ALTER TABLE offline_packages
        ADD COLUMN claim_generation INTEGER NOT NULL DEFAULT 0
        CHECK (claim_generation >= 0)";
pub(crate) const OFFLINE_RECOVERY_STATE_SCHEMA: &str = "ALTER TABLE offline_packages
        ADD COLUMN decoder_recovery_state TEXT NOT NULL DEFAULT 'primary'
        CHECK (decoder_recovery_state IN
            ('primary', 'recovery_pending', 'rehome_pending', 'alternate'))";
pub(crate) const OFFLINE_ALTERNATE_RECIPE_SCHEMA: &str = "ALTER TABLE offline_packages
        ADD COLUMN alternate_recipe_hash TEXT";
pub(crate) const CACHE_PUBLICATION_GENERATION_SCHEMA: &str = "ALTER TABLE transcode_cache_locations
        ADD COLUMN publication_generation INTEGER NOT NULL DEFAULT 0
        CHECK (publication_generation >= 0)";
pub(crate) const CACHE_PUBLICATION_GENERATION_GUARD_SCHEMA: &str =
    "CREATE TRIGGER cache_publication_generation_guard
    BEFORE UPDATE OF complete, bytes, manifest_digest, relative_dir, storage_id,
                     generation_id, publication_generation
    ON transcode_cache_locations
    WHEN NEW.complete = 1
         AND (OLD.complete != 1 OR NEW.bytes != OLD.bytes
              OR NEW.manifest_digest IS NOT OLD.manifest_digest
              OR NEW.relative_dir != OLD.relative_dir
              OR NEW.storage_id != OLD.storage_id
              OR NEW.generation_id != OLD.generation_id)
         AND NEW.publication_generation != OLD.publication_generation + 1
    BEGIN
        SELECT RAISE(ABORT, 'cache publication requires a current-generation writer');
    END";
pub(crate) const OFFLINE_CLAIM_LIFECYCLE_GUARD_SCHEMA: &str =
    "CREATE TRIGGER offline_claim_lifecycle_guard
    BEFORE UPDATE OF state, claim_generation ON offline_packages
    WHEN (OLD.state = 'queued' AND NEW.state = 'preparing'
            AND (NEW.claim_generation != OLD.claim_generation + 1
                 OR COALESCE((SELECT value FROM settings
                              WHERE key = 'offline.enabled'), '1')
                    IN ('0', 'false', 'off', 'no')))
      OR (OLD.state = 'queued' AND NEW.state = 'ready')
      OR (OLD.state = 'preparing' AND NEW.state IN ('queued', 'ready', 'failed')
            AND NEW.claim_generation != OLD.claim_generation + 1)
      OR (OLD.state IN ('queued', 'preparing') AND NEW.node_id != OLD.node_id
            AND NEW.claim_generation != OLD.claim_generation + 1)
    BEGIN
        SELECT RAISE(ABORT, 'invalid offline claim lifecycle transition');
    END";
pub(crate) const OFFLINE_RECOVERY_GUARD_SCHEMA: &str = "CREATE TRIGGER offline_recovery_guard
    BEFORE UPDATE OF recipe_hash, decoder_recovery_state, alternate_recipe_hash
    ON offline_packages
    WHEN (OLD.decoder_recovery_state = 'primary'
            AND NEW.decoder_recovery_state NOT IN ('primary', 'recovery_pending'))
      OR (OLD.decoder_recovery_state = 'recovery_pending'
            AND NEW.decoder_recovery_state NOT IN
                ('recovery_pending', 'rehome_pending', 'alternate'))
      OR (OLD.decoder_recovery_state = 'alternate'
            AND NEW.decoder_recovery_state NOT IN ('alternate', 'rehome_pending'))
      OR (OLD.decoder_recovery_state = 'rehome_pending'
            AND NEW.decoder_recovery_state NOT IN ('rehome_pending', 'alternate'))
      OR (OLD.decoder_recovery_state IN ('recovery_pending', 'alternate')
            AND NEW.decoder_recovery_state = 'rehome_pending'
            AND (NEW.node_id = OLD.node_id OR NEW.state != 'queued'
                 OR NEW.recipe_hash IS NOT NULL
                 OR NEW.alternate_recipe_hash IS NOT NULL))
      OR (OLD.alternate_recipe_hash IS NOT NULL
            AND NEW.alternate_recipe_hash IS NOT OLD.alternate_recipe_hash
            AND NOT (OLD.decoder_recovery_state = 'alternate'
                     AND NEW.decoder_recovery_state = 'rehome_pending'
                     AND NEW.node_id != OLD.node_id
                     AND NEW.state = 'queued'
                     AND NEW.recipe_hash IS NULL
                     AND NEW.alternate_recipe_hash IS NULL))
      OR (NEW.decoder_recovery_state = 'primary'
            AND NEW.alternate_recipe_hash IS NOT NULL)
      OR (NEW.decoder_recovery_state IN ('recovery_pending', 'rehome_pending')
            AND (NEW.recipe_hash IS NOT NULL OR NEW.alternate_recipe_hash IS NOT NULL))
      OR (NEW.decoder_recovery_state = 'alternate'
            AND (NEW.alternate_recipe_hash IS NULL
                 OR (NEW.recipe_hash IS NOT NULL
                     AND NEW.recipe_hash != NEW.alternate_recipe_hash)))
    BEGIN
        SELECT RAISE(ABORT, 'invalid offline decoder recovery transition');
    END";

/// The decoder-recovery budget and decision ledger, shared verbatim by both
/// backends.
///
/// One statement, for the same reason [`MEDIA_SESSION_PREPARATIONS_SCHEMA`] is
/// one statement: SQLite's append-only migration list keeps one element per
/// schema version and Hiqlite's `install_schema` keeps one per table, and both
/// spellings have to stay identical or a replicated import will disagree with
/// the node it imported from.
///
/// The primary key is `(user_id, playback_id, recovery_epoch)` and that is the
/// budget: one row per logical playback per epoch, so a retry cannot be granted
/// twice by presenting a new request id, a new client session, or a different
/// node. The epoch is server-owned and inherited by reopen, seek, track change
/// and handoff precisely so that none of those mints a fresh allowance.
///
/// This is a ledger, not a second session lifecycle. `state` moves
/// `reserved -> installed | exhausted` and never back; the row is never
/// deleted while the epoch lives, because deleting it *is* refunding the
/// budget. A crash between reserving and spawning therefore costs the
/// playback its one automatic recovery, which is the correct direction to
/// fail: an unbounded retry loop against a decoder that cannot decode the
/// file is worse than one lost attempt.
pub(crate) const MEDIA_SESSION_PRODUCER_RECOVERY_SCHEMA: &str =
    "CREATE TABLE IF NOT EXISTS media_session_producer_recovery (
        user_id                 INTEGER NOT NULL,
        playback_id             TEXT NOT NULL
            CHECK (length(playback_id) BETWEEN 1 AND 128),
        recovery_epoch          TEXT NOT NULL
            CHECK (length(recovery_epoch) BETWEEN 1 AND 128),
        failed_incarnation_id   TEXT NOT NULL,
        failed_producer_attempt INTEGER NOT NULL CHECK (failed_producer_attempt > 0),
        decision_sequence       INTEGER NOT NULL CHECK (decision_sequence > 0),
        failed_plan_digest      TEXT NOT NULL CHECK (length(failed_plan_digest) = 64),
        alternate_plan_digest   TEXT NOT NULL CHECK (length(alternate_plan_digest) = 64),
        decode_restriction      TEXT
            CHECK (decode_restriction IS NULL OR
                   length(CAST(decode_restriction AS BLOB)) <= 4096),
        state                   TEXT NOT NULL
            CHECK (state IN ('reserved', 'installed', 'exhausted')),
        created_at_ms           INTEGER NOT NULL,
        updated_at_ms           INTEGER NOT NULL,
        PRIMARY KEY (user_id, playback_id, recovery_epoch)
    ) STRICT;";

/// Field bounds the schema also enforces, checked here so a caller gets a typed
/// refusal instead of a constraint violation from three layers down — and
/// checked *once*, in one place both backends call. Two hand-copied copies of
/// these rules agree on the day they are written and nothing keeps them
/// agreeing afterwards, which on a dual-backend store is the whole hazard.
pub(crate) fn validated_recovery_request(
    request: &crate::domain::ProducerRecoveryRequest,
) -> Result<crate::domain::ProducerRecoveryRequest, StoreError> {
    let (user_id, playback_id, recovery_epoch) = validated_epoch_key(
        request.user_id,
        &request.playback_id,
        &request.recovery_epoch,
    )?;
    if request.failed_incarnation_id.is_empty() || request.failed_incarnation_id.len() > 128 {
        return Err(StoreError::Task("failed_incarnation_id".to_owned()));
    }
    // The actor counts attempts and sequences in `u64`; the column is a signed
    // integer. A silent wrap would put a later attempt behind an earlier one.
    let failed_producer_attempt = checked_recovery_counter(request.failed_producer_attempt)?;
    let decision_sequence = checked_recovery_counter(request.decision_sequence)?;
    if failed_producer_attempt <= 0 || decision_sequence <= 0 {
        return Err(StoreError::Task(
            "producer attempt and decision sequence are one-based".to_owned(),
        ));
    }
    for (name, digest) in [
        ("failed_plan_digest", &request.failed_plan_digest),
        ("alternate_plan_digest", &request.alternate_plan_digest),
    ] {
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(StoreError::Task(name.to_owned()));
        }
    }
    // An alternative that names the same plan is not an alternative.
    if request.failed_plan_digest == request.alternate_plan_digest {
        return Err(StoreError::Task(
            "the alternate plan is the failed plan".to_owned(),
        ));
    }
    Ok(crate::domain::ProducerRecoveryRequest {
        user_id,
        playback_id,
        recovery_epoch,
        failed_incarnation_id: request.failed_incarnation_id.clone(),
        failed_producer_attempt: request.failed_producer_attempt,
        decision_sequence: request.decision_sequence,
        failed_plan_digest: request.failed_plan_digest.clone(),
        alternate_plan_digest: request.alternate_plan_digest.clone(),
        decode_restriction: request.decode_restriction.clone(),
    })
}

pub(crate) fn validated_epoch_key(
    user_id: i64,
    playback_id: &str,
    recovery_epoch: &str,
) -> Result<(i64, String, String), StoreError> {
    if user_id <= 0 {
        return Err(StoreError::Task("user_id".to_owned()));
    }
    if playback_id.is_empty() || playback_id.len() > 128 {
        return Err(StoreError::Task("playback_id".to_owned()));
    }
    if recovery_epoch.is_empty() || recovery_epoch.len() > 128 {
        return Err(StoreError::Task("recovery_epoch".to_owned()));
    }
    Ok((user_id, playback_id.to_owned(), recovery_epoch.to_owned()))
}

/// The actor counts in `u64`; the column is signed. Refuse rather than wrap.
pub(crate) fn checked_recovery_counter(value: u64) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| StoreError::Task("recovery counter overflows".to_owned()))
}

pub(crate) fn encoded_recovery_restriction(
    request: &crate::domain::ProducerRecoveryRequest,
) -> Result<Option<String>, StoreError> {
    request
        .decode_restriction
        .as_ref()
        .map(|restriction| {
            restriction
                .encode()
                .map_err(|error| StoreError::Task(error.to_string()))
        })
        .transpose()
}

/// Turn one stored ledger row into a reservation, or refuse it.
///
/// Shared so that the two backends cannot answer differently for the same
/// bytes. A `decode_restriction` that will not parse is not an absent
/// restriction — treating it as absent restores automatic hardware selection
/// for a source that already failed on hardware — so it is a
/// [`StoreError::Task`] on both, not a `rusqlite` conversion failure on one and
/// a task error on the other.
#[allow(clippy::too_many_arguments)]
pub(crate) fn recovery_reservation_from_row(
    user_id: i64,
    playback_id: &str,
    recovery_epoch: &str,
    failed_incarnation_id: String,
    failed_producer_attempt: i64,
    decision_sequence: i64,
    failed_plan_digest: String,
    alternate_plan_digest: String,
    stored_restriction: Option<String>,
    state: &str,
    created_at_ms: i64,
    updated_at_ms: i64,
) -> Result<crate::domain::ProducerRecoveryReservation, StoreError> {
    let decode_restriction = match stored_restriction {
        Some(stored) => Some(
            crate::domain::ContinuationDecodeRestriction::decode(&stored).map_err(|error| {
                StoreError::Task(format!("stored decode restriction is unreadable: {error}"))
            })?,
        ),
        None => None,
    };
    let state = crate::domain::ProducerRecoveryState::parse(state)
        .ok_or_else(|| StoreError::Task(format!("unknown recovery state {state}")))?;
    Ok(crate::domain::ProducerRecoveryReservation {
        user_id,
        playback_id: playback_id.to_owned(),
        recovery_epoch: recovery_epoch.to_owned(),
        failed_incarnation_id,
        // A negative stored counter is a row this build did not write. The
        // write path refuses a silent wrap; the read path has to as well, or
        // the refusal is only half a rule.
        failed_producer_attempt: u64::try_from(failed_producer_attempt)
            .map_err(|_| StoreError::Task("stored producer attempt is negative".to_owned()))?,
        decision_sequence: u64::try_from(decision_sequence)
            .map_err(|_| StoreError::Task("stored decision sequence is negative".to_owned()))?,
        failed_plan_digest,
        alternate_plan_digest,
        decode_restriction,
        state,
        created_at_ms,
        updated_at_ms,
    })
}

/// What a viewer has asked for, per playback, shared verbatim by both backends.
///
/// Everything else about a playback records what *happened*: which incarnation
/// is current, what was built, how far it produced, what the response said.
/// This is the only row that records what was *wanted*, and the distinction is
/// the whole reason it exists. A resolved height cannot tell Original from a
/// manual pick at the source's own height; a session's recipe is desired-as-
/// built and is written once at activation; the durable intent fingerprint is
/// deliberately lossy so a retry recovers the first answer. None of the three
/// can answer "has the viewer asked for something else since", which is the
/// question every admission point has to ask before it publishes media.
///
/// Keyed by playback rather than by session, and kept out of
/// `media_playback_pointers` for the reason that matters here: a desired row
/// has to be able to exist *before* the session that satisfies it, and that
/// table's `current_incarnation_id` is `NOT NULL`. An ask that only becomes
/// durable once something has already been built for it cannot be the thing
/// that decides whether to build it.
///
/// `revision` advances only when `digest` changes, which is what makes it a
/// media-intent revision rather than a cadence counter — the four near misses
/// §1 names all move on heartbeats, reporter attachments or recovery. It is
/// monotone per playback so two asks are orderable, which a content hash alone
/// can never be.
///
/// `canonical_form` is stored beside the digest deliberately, at the cost of
/// about a hundred bytes. A digest whose input is not recoverable is a hash
/// nobody can check: an operator reading this row can see what the viewer
/// asked for, and a later binary can verify that its own canonical form still
/// produces the stored digest rather than discovering a silent mismatch when
/// every comparison starts failing.
pub(crate) const MEDIA_PLAYBACK_DESIRED_SCHEMA: &str =
    "CREATE TABLE IF NOT EXISTS media_playback_desired (
        user_id        INTEGER NOT NULL,
        playback_id    TEXT NOT NULL
            CHECK (length(playback_id) BETWEEN 1 AND 128),
        revision       INTEGER NOT NULL CHECK (revision > 0),
        digest         TEXT NOT NULL CHECK (length(digest) = 64),
        canonical_form TEXT NOT NULL
            CHECK (length(canonical_form) BETWEEN 1 AND 512),
        updated_at_ms  INTEGER NOT NULL,
        PRIMARY KEY (user_id, playback_id)
    ) STRICT;";

/// Fence a playback pointer written against an ask that is not the current one.
///
/// Every comparison of the desired revision up to now has been application SQL
/// — the predicate preparation admission, prepared commit and ordinary
/// activation each carry. That fences *this* binary against a stale ask and
/// does nothing at all about an older one, because an older binary simply does
/// not emit the predicate. Schema compatibility is checked when a database is
/// opened and never again, so a process that was already running when the
/// cluster migrated keeps writing this pointer with statements from before the
/// rule existed. A trigger is the only thing standing in the path of a
/// statement this binary did not write.
///
/// The column and both triggers are one step because they are one change. A
/// column nothing enforces is not a fence, and a trigger cannot be created on
/// a column that does not exist yet; a database holding one without the other
/// accepts exactly the writes this exists to refuse. The replicated chain has
/// already spent a whole extra version step recovering from a trigger that
/// shipped separately from the column it guards, which is the mistake being
/// avoided here rather than repeated.
///
/// `desired_revision` is nullable and the null carries meaning: it is what a
/// write leaves when the playback has no recorded ask. Every writer in this
/// binary fills it from `media_playback_desired` inside the same statement, so
/// a null on a playback that *does* have an ask can only have come from a
/// binary that predates the column — which is precisely the writer being
/// fenced. `IS NOT` rather than `!=` because a null compared with `!=` is
/// null, not true, and the old writer would sail through the one comparison
/// written for it.
///
/// Both operations, because an upsert reaches the pointer through both: a
/// first pointer inserts, a replacement updates. A fence on one of them is a
/// fence on nothing, since the case that matters — an old writer replacing a
/// live pointer — is the one that updates.
///
/// A playback with no ask row is untouched, deliberately. "No ask recorded" is
/// not "a different ask", the first play of every title looks like this, and
/// an import loads pointers before it loads asks. `RAISE(ABORT)` rather than a
/// silent no-op, because an old writer that believes it advanced the pointer
/// is the whole failure being prevented, and a refusal it cannot see is not a
/// fence.
macro_rules! pointer_desired_revision_column {
    () => {
        // No trailing semicolon. The replicated backend submits this as one
        // prepared statement and a trailing `;` makes it unpreparable — which
        // is exactly how the three-voter lane failed every migration test in
        // the chain while the SQLite batch, where the semicolon was needed,
        // passed. The batch below adds its own separator instead.
        "ALTER TABLE media_playback_pointers ADD COLUMN desired_revision INTEGER"
    };
}

macro_rules! pointer_desired_fence_insert_trigger {
    () => {
        "CREATE TRIGGER IF NOT EXISTS media_playback_pointers_desired_fence_ai
    BEFORE INSERT ON media_playback_pointers
    WHEN EXISTS (SELECT 1 FROM media_playback_desired
                  WHERE user_id = NEW.user_id AND playback_id = NEW.playback_id
                    AND revision IS NOT NEW.desired_revision)
    BEGIN
      SELECT RAISE(ABORT, 'playback pointer written against an ask that is not current');
    END;"
    };
}

macro_rules! pointer_desired_fence_update_trigger {
    () => {
        "CREATE TRIGGER IF NOT EXISTS media_playback_pointers_desired_fence_au
    BEFORE UPDATE ON media_playback_pointers
    WHEN EXISTS (SELECT 1 FROM media_playback_desired
                  WHERE user_id = NEW.user_id AND playback_id = NEW.playback_id
                    AND revision IS NOT NEW.desired_revision)
    BEGIN
      SELECT RAISE(ABORT, 'playback pointer written against an ask that is not current');
    END;"
    };
}

/// The three statements as one SQLite migration, applied in one transaction.
///
/// Written through macros rather than by hand twice because the replicated
/// backend needs them as separate transaction entries and this one needs them
/// as a batch — and the two backends holding different text for the same
/// guard is the failure every shared schema constant here exists to prevent.
pub(crate) const MEDIA_PLAYBACK_POINTER_DESIRED_FENCE_SCHEMA: &str = concat!(
    pointer_desired_revision_column!(),
    ";\n",
    pointer_desired_fence_insert_trigger!(),
    "\n",
    pointer_desired_fence_update_trigger!(),
);

/// The column on its own, for the replicated migration's first statement.
pub(crate) const MEDIA_PLAYBACK_POINTER_DESIRED_REVISION_COLUMN: &str =
    pointer_desired_revision_column!();

/// The insert-path trigger on its own.
pub(crate) const MEDIA_PLAYBACK_POINTER_DESIRED_FENCE_INSERT_TRIGGER: &str =
    pointer_desired_fence_insert_trigger!();

/// The update-path trigger on its own.
pub(crate) const MEDIA_PLAYBACK_POINTER_DESIRED_FENCE_UPDATE_TRIGGER: &str =
    pointer_desired_fence_update_trigger!();

/// When a predecessor kept alive on purpose stops being kept.
///
/// Null means "not draining", which is every row that exists today and every
/// row a session starts life as. A non-null value is a deadline in the same
/// milliseconds every other clock column here uses: past it, the session is
/// over regardless of what its lease says.
///
/// The column exists because three earlier attempts tried to *infer* "this
/// session is draining" — from the playback pointer, and through the session
/// lease — and each inference was already carrying a different meaning for
/// somebody else. The pointer is deleted by an ordinary viewer action, so a
/// drain keyed on it never ends; the lease is what the owner's renewal loop
/// reads as liveness, so a drain keyed on it kills the worker in one tick.
/// Neither is a fact about draining. This is, and it is durable, so it
/// survives the restart of the node that set it and is visible to the node
/// that inherits the work when that node does not come back.
///
/// Deliberately nullable rather than `NOT NULL DEFAULT 0`: a zero would be a
/// deadline in 1970, and every reader would have to spell out that zero is not
/// really a deadline. The null says it once, in the schema.
macro_rules! media_session_drain_deadline_column {
    () => {
        // No trailing semicolon. The replicated backend submits this as one
        // prepared statement and a trailing `;` makes it unpreparable — the
        // failure the pointer-fence step above documents at length.
        "ALTER TABLE media_sessions ADD COLUMN drain_deadline_ms INTEGER"
    };
}

/// Refuse to move a draining session's ownership.
///
/// Application SQL already excludes a non-null deadline from both halves of
/// takeover — the expired-route inventory and the CAS. That fences *this*
/// binary and does nothing at all about an older one, because an older binary
/// simply does not emit the predicate. Schema compatibility is checked when a
/// database is opened and never again, so during a rolling upgrade every node
/// still running the previous binary keeps issuing takeover SQL from before
/// this column existed, against a store that already has it.
///
/// What that costs without the trigger is the exact failure the whole drain
/// design was rewritten to avoid. A new-binary owner dies mid-drain, its lease
/// lapses, and an old-binary node adopts the predecessor — a stream the
/// pointer no longer names — starts an encoder for it, and then renews it
/// every three seconds for as long as that node lives. Nothing on the old node
/// ends it, because nothing on the old node knows it is draining. An immortal
/// predecessor holding an encoder, an admission permit and a shared-cache
/// generation, arriving through the upgrade door.
///
/// `RAISE(IGNORE)` rather than `RAISE(ABORT)`, which is the opposite of the
/// pointer fence above and for a reason. Takeover's CAS already checks that it
/// updated exactly one row and rolls the whole transaction back when it did
/// not — that is how it handles losing a race to another survivor. Making the
/// update affect no rows therefore lands the old binary in a path it already
/// implements correctly, and it lands there quietly, once per attempt, instead
/// of raising an error it would log and retry forever. The pointer fence
/// chose `ABORT` because its old writer had no such check and would have
/// believed it succeeded.
///
/// Guarded on the ownership columns only. The owner renews the lease on this
/// row every three seconds for the whole drain, deliberately, and the sweep
/// and the owner's own end both write `state` — none of those move ownership,
/// and none of them may be fenced.
macro_rules! media_session_drain_ownership_fence_trigger {
    () => {
        "CREATE TRIGGER IF NOT EXISTS media_sessions_drain_ownership_fence_au
    BEFORE UPDATE OF owner_node_id, owner_epoch ON media_sessions
    WHEN OLD.drain_deadline_ms IS NOT NULL
     AND (NEW.owner_node_id IS NOT OLD.owner_node_id
          OR NEW.owner_epoch IS NOT OLD.owner_epoch)
    BEGIN
      SELECT RAISE(IGNORE);
    END;"
    };
}

/// The drain deadline column, for the replicated migration's first statement.
pub(crate) const MEDIA_SESSION_DRAIN_DEADLINE_COLUMN: &str = media_session_drain_deadline_column!();

/// The ownership fence on its own, for the replicated migration and install.
pub(crate) const MEDIA_SESSION_DRAIN_OWNERSHIP_FENCE_TRIGGER: &str =
    media_session_drain_ownership_fence_trigger!();

/// Column and fence as one SQLite migration, applied in one transaction.
///
/// One step, for the reason the pointer fence above spells out: a database
/// holding the column without the trigger accepts exactly the writes the
/// trigger exists to refuse, and that database is the middle of every rolling
/// upgrade. The trigger cannot be created before the column exists, so they
/// cannot be two steps.
pub(crate) const MEDIA_SESSION_DRAIN_DEADLINE_SCHEMA: &str = concat!(
    media_session_drain_deadline_column!(),
    ";\n",
    media_session_drain_ownership_fence_trigger!(),
);

const MEDIA_SESSION_PUBLICATION_CLAIM_TRIGGER_SCHEMA: &str =
    "CREATE TRIGGER IF NOT EXISTS media_session_publication_claim_au
    AFTER UPDATE OF publication_ready_at_ms ON media_sessions
    WHEN NEW.state = 'active' AND (
      (OLD.publication_ready_at_ms = 9223372036854775807
        AND NEW.publication_ready_at_ms != 9223372036854775807)
      OR (OLD.publication_ready_at_ms > 0
        AND OLD.publication_ready_at_ms < 9223372036854775807
        AND NEW.publication_ready_at_ms = 0)
    ) BEGIN
      UPDATE media_session_requests
         SET claim_expires_at_ms = CASE
               WHEN OLD.publication_ready_at_ms = 9223372036854775807
                    AND NEW.publication_ready_at_ms = 0
                 THEN NEW.lease_expires_at_ms
               WHEN OLD.publication_ready_at_ms = 9223372036854775807
                 THEN MIN(9223372036854775806,
                   NEW.publication_ready_at_ms
                     + (NEW.lease_expires_at_ms - OLD.updated_at_ms) + 1)
               ELSE MIN(9223372036854775806, NEW.lease_expires_at_ms + 1)
             END,
             updated_at_ms = NEW.updated_at_ms
       WHERE incarnation_id = NEW.incarnation_id
         AND request_fingerprint = NEW.request_fingerprint
         AND playback_id = NEW.playback_id
         AND owner_node_id = NEW.owner_node_id
         AND state = 'starting';
    END;";

#[cfg(feature = "hiqlite-store")]
pub use self::hiqlite::{
    prometheus_store_operations, ClusterCompatibility, HiqliteAuthStore, AUTH_LEARNER_PROTOCOL,
    AUTH_PROTOCOL_MAX, AUTH_PROTOCOL_MIN, AUTH_PROTOCOL_VERSION, AUTH_SCHEMA_MIGRATION_SOURCE,
    AUTH_SCHEMA_VERSION,
};
#[cfg(feature = "cluster-read-cost-validation")]
pub use self::hiqlite::{
    validation_set_store_operation_instrumentation,
    validation_store_operation_instrumentation_enabled, validation_store_operation_metric_count,
    HiqliteOperationCounts,
};
#[cfg(feature = "hiqlite-store")]
pub use self::hiqlite_import::{SqliteImportReport, SqliteImportTableDigest};
pub use fragment_index_cluster::{
    analysis_backoff_ms, bounded_analysis_backoff_base_secs, bounded_analysis_backoff_max_secs,
    bounded_analysis_lease_secs, bounded_analysis_max_attempts, bounded_subtitle_window_seconds,
    cluster_fragment_index_blob_sha256, cluster_fragment_index_generation_key,
    cluster_fragment_index_key, cluster_fragment_index_pipeline_digest,
    decode_cluster_fragment_index_blob, encode_cluster_fragment_index_blob, stored_switch,
    AnalysisAttempt, AnalysisFileLabel, AnalysisHistoryCursor, AnalysisHistoryFilter,
    AnalysisHistoryPage, AnalysisHistoryQuery, AnalysisHistoryRow, AnalysisIndexRepairCandidate,
    AnalysisIndexRepairResult, AnalysisRequest, AnalysisStatusSummary,
    ClusterFragmentIndexArtifact, ClusterFragmentIndexFailure, ClusterFragmentIndexJob,
    ClusterFragmentIndexLocation, ClusterFragmentIndexStore, FragmentIndexSourceObservation,
    NewAnalysisRequest, NewClusterFragmentIndexJob, SubtitleBackfillCandidate,
    SubtitleBackfillDiagnostics, SubtitleSourcePublication, SubtitleSourceStamp,
    CONTENT_ANALYSIS_REPAIR_HEADROOM, CONTENT_ANALYSIS_REPAIR_MAX_CANDIDATES,
    CONTENT_ANALYSIS_REPAIR_REVISION, DEFAULT_ANALYSIS_BACKOFF_BASE_SECS,
    DEFAULT_ANALYSIS_BACKOFF_MAX_SECS, DEFAULT_ANALYSIS_LEASE_SECS, DEFAULT_ANALYSIS_MAX_ATTEMPTS,
    DEFAULT_SUBTITLE_WINDOW_SECS, MAX_ACTIVE_ANALYSIS_REQUESTS, MAX_ANALYSIS_BACKOFF_BASE_SECS,
    MAX_ANALYSIS_BACKOFF_MAX_SECS, MAX_ANALYSIS_LEASE_SECS, MAX_ANALYSIS_MAX_ATTEMPTS,
    MAX_CLUSTER_FRAGMENT_INDEX_BLOB_BYTES, MAX_SUBTITLE_WINDOW_SECS, MIN_SUBTITLE_WINDOW_SECS,
    SUBTITLE_SOURCE_REPAIR_LIMIT, SUBTITLE_SOURCE_REPAIR_WINDOW_MS,
};
pub use publication::{PublicationFence, PublicationStore};
pub use sqlite::{prometheus_sqlite_health, SqliteStore, SQLITE_SCHEMA_VERSION};

use async_trait::async_trait;

use crate::cluster::coordination::{Lease, LeaseClaim};
use crate::domain::{
    BookMetadataPatch, CacheConsumerKind, CacheConsumerPin, CacheManifestCheck, CacheStorageMember,
    CachedTranscode, DolbyVisionFacts, HomePreviewPage, InProgressItem, Item, ItemEdit, ItemKind,
    ItemPage, ItemSort, Library, MediaFile, MediaSessionActivation, MediaSessionActivationOutcome,
    MediaSessionActivationSettlement, MediaSessionProjectionCompletion, MediaSessionRenewal,
    MediaSessionRequestClaim, MediaSessionRoute, MediaSessionTakeover, MediaShape, MetadataPatch,
    NetworkPrior, NetworkPriorObservation, NewItem, NewLibrary, NewOfflinePackage,
    NewPretranscodeJob, OfflineActivityPackage, OfflineCreateOutcome, OfflineLeaseOutcome,
    OfflinePackage, OfflinePackageStats, OfflineRemovalPlanEntry, OfflineRemovalReport,
    OwnedMediaSessionLease, PlaybackEvent, PlaybackEventQuery, PretranscodeJob,
    PretranscodeWorkerCapabilities, ProbeResult, ReadingState, ReadingStateWrite, RecentItem,
    SharedCacheGeneration, TraktAuth, User, WatchRollup, WatchState,
};
// RecentItem is reused for next-up (episode + show title).
use crate::error::StoreError;
use crate::mediafacts::MediaFacts;
use crate::secrets::SealedSecret;

/// Narrow catalogue projection used to reconcile node-local artwork bytes.
/// Large overview and metadata fields never cross the local database boundary
/// for a pass that needs only ownership and filenames.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtworkInventoryItem {
    pub id: i64,
    pub poster_path: Option<String>,
    pub backdrop_path: Option<String>,
}

/// One stored-probe row eligible for the sample-entry backfill.
///
/// Every source identity field and the exact probe snapshot participate in
/// the guarded update, so a concurrent scan or replacement cannot be
/// overwritten by facts parsed from an older file revision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MissingVideoCodecTag {
    pub id: i64,
    pub path: String,
    pub size: i64,
    pub mtime: i64,
    pub probe_json: String,
}

/// One exact stored-probe snapshot eligible for the field-order backfill.
///
/// The source identity fields fence the update against a concurrent rescan in
/// exactly the same way as [`MissingVideoCodecTag`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MissingFieldOrder {
    pub id: i64,
    pub path: String,
    pub size: i64,
    pub mtime: i64,
    pub probe_json: String,
}

/// Result of applying an externally supplied series identifier to one show.
/// The operation is deliberately narrower than general metadata updates: a
/// known, different provider ID is diagnostic evidence, never permission to
/// overwrite it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeriesHintOutcome {
    Applied,
    AlreadyEqual,
    Conflict { current_tmdb_id: i64 },
    MissingOrWrongKind,
}

/// Lexical file-path range for a canonical directory, using the separator
/// already present in the stored representation. The upper bound is the
/// successor of the trailing separator: `/` becomes `0`, while `\` becomes
/// `]`. This keeps the lookup indexable without treating SQL LIKE metacharacters
/// or path case as special.
pub(crate) fn directory_path_bounds(directory: &str) -> Result<(String, String), StoreError> {
    let trimmed = normalized_directory(directory)?;
    let separator = if trimmed.contains('/') { '/' } else { '\\' };
    let lower = format!("{trimmed}{separator}");
    let mut upper = trimmed.to_owned();
    upper.push(match separator {
        '/' => '0',
        '\\' => ']',
        _ => unreachable!("path separator is fixed above"),
    });
    Ok((lower, upper))
}

pub(crate) fn normalized_directory(directory: &str) -> Result<&str, StoreError> {
    let separator = if directory.contains('/') {
        '/'
    } else if directory.contains('\\') {
        '\\'
    } else {
        return Err(StoreError::Database(format!(
            "invalid canonical directory prefix `{directory}`"
        )));
    };
    // Trim only the stored path's separator. A backslash is a valid POSIX
    // filename character and must not be silently erased from that identity.
    let trimmed = directory.trim_end_matches(separator);
    if trimmed.is_empty() || (!trimmed.contains('/') && !trimmed.contains('\\')) {
        return Err(StoreError::Database(format!(
            "invalid canonical directory prefix `{directory}`"
        )));
    }
    Ok(trimmed)
}

pub(crate) fn directory_matches_show_path(path: &str, directory: &str) -> bool {
    let path = std::path::Path::new(path);
    crate::scan::parse::parse_episode(path)
        .or_else(|_| crate::scan::parse::parse_anime_episode(path))
        .ok()
        .and_then(|parsed| parsed.source_directory)
        .is_some_and(|candidate| candidate.to_string_lossy() == directory)
}

pub(crate) fn directory_matches_movie_path(path: &str, directory: &str) -> bool {
    crate::scan::parse::parse_movie(std::path::Path::new(path))
        .source_directory
        .is_some_and(|candidate| candidate.to_string_lossy() == directory)
}

#[cfg(test)]
mod scan_identity_path_tests {
    use super::directory_path_bounds;

    #[test]
    fn scan_identity_directory_bounds_follow_the_stored_separator() {
        assert_eq!(
            directory_path_bounds("/media/Show_Name%/ ").expect("posix bounds"),
            (
                "/media/Show_Name%/ /".to_owned(),
                "/media/Show_Name%/ 0".to_owned()
            )
        );
        assert_eq!(
            directory_path_bounds(r"C:\media\Show_Name%\").expect("windows bounds"),
            (
                r"C:\media\Show_Name%\".to_owned(),
                r"C:\media\Show_Name%]".to_owned()
            )
        );
        assert!(directory_path_bounds("").is_err());
        assert!(directory_path_bounds("relative").is_err());
    }
}

/// One bounded aggregate read for Store-backed Prometheus gauges.
///
/// Keeping this as one Store primitive lets a replicated backend pay for one
/// authority read per background sample instead of one read per metric family.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PrometheusStoreSnapshot {
    pub libraries: i64,
    pub users: i64,
    pub offline: OfflinePackageStats,
    pub watched_outbox: (i64, i64, i64),
    pub analysis: AnalysisStoreMetrics,
}

pub const ANALYSIS_METRIC_COMPONENTS: [&str; 3] =
    ["fragment_index", "skip_markers", "subtitle_source"];
pub const ANALYSIS_METRIC_STATES: [&str; 8] = [
    "queued",
    "claimed",
    "retry_wait",
    "staged",
    "published",
    "failed",
    "canceled",
    "stale",
];
pub const ANALYSIS_METRIC_PRIORITIES: [&str; 3] = ["normal", "forced", "foreground"];
pub const ANALYSIS_METRIC_TRIGGERS: [&str; 4] = ["admin", "background", "foreground", "playback"];
pub const ANALYSIS_MARKER_KINDS: [&str; 4] = ["intro", "recap", "credits", "preview"];
pub const ANALYSIS_MARKER_PROVENANCE: [&str; 4] = ["estimated", "detected", "authored", "manual"];
pub const ANALYSIS_MARKER_CONFIDENCE: [&str; 3] = ["low", "medium", "high"];
/// Complete, bounded label universe for retained analysis lifecycle gauges.
/// Store error strings are classified into these slots before they reach
/// Prometheus, so paths or other unbounded values can never become labels.
pub const ANALYSIS_LIFECYCLE_METRICS: [(&str, &str); 27] = [
    ("claim", "all"),
    ("lease_loss", "lease_expired"),
    ("retry", "lease_expired"),
    ("retry", "source_catalog_read_failed"),
    ("retry", "source_probe_timeout"),
    ("retry", "source_unavailable"),
    ("retry", "source_attestation_failed"),
    ("retry", "foreground_preempted"),
    ("retry", "source_attestation_timeout"),
    ("retry", "source_record_failed"),
    ("retry", "queue_write_failed"),
    ("retry", "queue_full_or_busy"),
    ("retry", "pipeline_version_unavailable"),
    ("retry", "other"),
    ("cancel", "admin_cancelled"),
    ("cancel", "source_deleted"),
    ("cancel", "other"),
    ("stale", "source_identity_changed"),
    ("failure", "attempt_limit"),
    ("failure", "pipeline_version_unavailable"),
    ("failure", "stored_probe_invalid"),
    ("failure", "source_duration_missing"),
    ("failure", "invalid_cache_identity"),
    ("failure", "unsupported"),
    ("failure", "source_unavailable"),
    ("failure", "other"),
    ("publication", "validated"),
];
pub const ANALYSIS_QUEUE_METRIC_SLOTS: usize = ANALYSIS_METRIC_COMPONENTS.len()
    * ANALYSIS_METRIC_STATES.len()
    * ANALYSIS_METRIC_PRIORITIES.len()
    * ANALYSIS_METRIC_TRIGGERS.len();
pub const ANALYSIS_MARKER_METRIC_SLOTS: usize = ANALYSIS_MARKER_KINDS.len()
    * ANALYSIS_MARKER_PROVENANCE.len()
    * ANALYSIS_MARKER_CONFIDENCE.len();
pub const ANALYSIS_LIFECYCLE_METRIC_SLOTS: usize = ANALYSIS_LIFECYCLE_METRICS.len();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnalysisStoreMetrics {
    pub queue_depth: [i64; ANALYSIS_QUEUE_METRIC_SLOTS],
    pub queue_oldest_age_seconds: [i64; ANALYSIS_QUEUE_METRIC_SLOTS],
    pub lifecycle_counts: [i64; ANALYSIS_LIFECYCLE_METRIC_SLOTS],
    pub marker_counts: [i64; ANALYSIS_MARKER_METRIC_SLOTS],
    pub health: AnalysisQueueHealth,
}

impl Default for AnalysisStoreMetrics {
    fn default() -> Self {
        Self {
            queue_depth: [0; ANALYSIS_QUEUE_METRIC_SLOTS],
            queue_oldest_age_seconds: [0; ANALYSIS_QUEUE_METRIC_SLOTS],
            lifecycle_counts: [0; ANALYSIS_LIFECYCLE_METRIC_SLOTS],
            marker_counts: [0; ANALYSIS_MARKER_METRIC_SLOTS],
            health: AnalysisQueueHealth::default(),
        }
    }
}

/// What the fragment-index queue has actually produced lately.
///
/// The jobs table cannot answer "how many claims in 24 hours": a row keeps
/// only its latest transition, so a row claimed five times is one row. These
/// are the questions it *can* answer cheaply, and the cumulative lifecycle
/// counters supply the rest.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AnalysisQueueHealth {
    /// Jobs that reached `ready` inside the window. `ready` rather than
    /// artifact rows, because a hydrated completion produces a job and a
    /// location but no new artifact, and a healthy hydrating fleet must not
    /// read as dead.
    pub ready_24h: i64,
    pub attempt_limit_24h: i64,
    /// Jobs the queue has actually picked up inside the window — `attempts`
    /// is charged on claim, so a row with any attempt was claimed at least
    /// once. This is the denominator the failure ratio needs: the cumulative
    /// `('claim','all')` lifecycle counter is shared with `skip_markers`,
    /// so a busy marker pipeline would otherwise vouch for a dead index.
    pub claimed_24h: i64,
    /// Fragment-index rows that are `queued` and past `not_before_ms`: work
    /// the queue could claim right now and has not. A standing backlog with
    /// no claims is the silent stall this verdict exists to name, and it is
    /// the one shape a claim-counting rule alone reads as `idle`.
    pub claimable: i64,
    /// Rows sitting `running` past a lease nobody renewed. One is a sweep
    /// that has not run yet; a standing count is the outage's own signature.
    pub running_past_lease: i64,
    pub last_ready_at_ms: i64,
}

/// The one-word answer an operator needs before reading anything else.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnalysisQueueVerdict {
    /// Nothing to judge: nothing has been claimed and nothing has finished.
    /// A fully indexed library and a paused one look the same from here, and
    /// neither is a fault.
    Idle,
    Healthy,
    Degraded,
    /// Claimed enough to be sure, produced nothing.
    Dead,
}

impl AnalysisQueueVerdict {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Healthy => "healthy",
            Self::Degraded => "degraded",
            Self::Dead => "dead",
        }
    }
}

pub const ANALYSIS_QUEUE_VERDICTS: [&str; 4] = ["idle", "healthy", "degraded", "dead"];

/// Twenty claims is enough that "no successes" is not a small sample. On the
/// fleet that produced this rule it was reached inside the first hour of a
/// seventy-two hour outage.
pub const ANALYSIS_DEAD_CLAIM_FLOOR: i64 = 20;

/// The same floor for the other shape of the same outage: work waiting and
/// nobody picking it up. Twenty claimable rows is past any rounding — the
/// stalled fleet stood at four figures.
pub const ANALYSIS_DEAD_BACKLOG_FLOOR: i64 = 20;

/// Minimum observed claims before the lease-loss *ratio* is allowed to speak.
/// That ratio's denominator is a per-process delta, so on a young daemon four
/// claims and one preemption would otherwise read as a quarter lost.
pub const ANALYSIS_RATIO_CLAIM_FLOOR: i64 = 8;

/// Read the queue's verdict.
///
/// Everything the verdict *rules on* comes from `health`, which is
/// fragment-index-scoped and window-scoped on both sides of every ratio. The
/// two `since_start` figures are per-process deltas over a counter shared with
/// `skip_markers`, so they only feed the lease-loss ratio — the one question
/// the jobs table cannot answer — and only once there are enough of them.
///
/// Every input is fleet-wide: `cluster_fragment_index_jobs` is replicated and
/// carries no "which node observed this" column, so two nodes reading the same
/// term return the same verdict. It says whether *the queue* is producing, not
/// whether this node is.
#[must_use]
pub fn analysis_queue_verdict(
    health: AnalysisQueueHealth,
    claims_since_start: i64,
    lease_losses_since_start: i64,
    running_past_lease_samples: u32,
) -> AnalysisQueueVerdict {
    let nothing_happening = health.ready_24h == 0
        && health.claimed_24h == 0
        && health.claimable == 0
        && health.running_past_lease == 0;
    if nothing_happening {
        return AnalysisQueueVerdict::Idle;
    }
    if health.ready_24h == 0 {
        // Two ways to be sure, and a wedged queue only ever shows one of
        // them: it either claimed plenty and produced nothing, or it never
        // claimed at all while work piled up in front of it.
        let claimed_enough_to_be_sure = health.claimed_24h >= ANALYSIS_DEAD_CLAIM_FLOOR;
        let backlog_nobody_touched =
            health.claimed_24h == 0 && health.claimable >= ANALYSIS_DEAD_BACKLOG_FLOOR;
        return if claimed_enough_to_be_sure || backlog_nobody_touched {
            AnalysisQueueVerdict::Dead
        } else {
            // Something is in front of the queue and nothing has come out of
            // it, but not enough of either to call it. Not healthy, and not
            // yet provable.
            AnalysisQueueVerdict::Degraded
        };
    }
    // A quarter is generous on purpose: preemption by foreground playback is a
    // legitimate way to lose a lease.
    let losses_over_budget = lease_losses_since_start > 0
        && claims_since_start >= ANALYSIS_RATIO_CLAIM_FLOOR
        && lease_losses_since_start * 4 >= claims_since_start;
    // A tenth of what the queue picked up exhausting its retry budget. Both
    // sides are the same 24 hours over the same table, so a restart moves
    // neither and a quiet night shrinks both together.
    let failures_over_budget =
        health.attempt_limit_24h > 0 && health.attempt_limit_24h * 10 >= health.claimed_24h;
    if losses_over_budget || failures_over_budget || running_past_lease_samples >= 2 {
        return AnalysisQueueVerdict::Degraded;
    }
    AnalysisQueueVerdict::Healthy
}

#[derive(serde::Deserialize)]
struct AnalysisQueueMetricRow {
    component: String,
    state: String,
    priority: String,
    trigger: String,
    count: i64,
    oldest: i64,
}

#[derive(serde::Deserialize)]
struct AnalysisMarkerMetricRow {
    kind: String,
    provenance: String,
    confidence: String,
    count: i64,
}

#[derive(serde::Deserialize)]
struct AnalysisLifecycleMetricRow {
    event: String,
    reason: String,
    count: i64,
}

#[cfg(test)]
mod queue_verdict_tests {
    use super::{
        analysis_queue_verdict, AnalysisQueueHealth, AnalysisQueueVerdict,
        ANALYSIS_DEAD_BACKLOG_FLOOR, ANALYSIS_DEAD_CLAIM_FLOOR, ANALYSIS_RATIO_CLAIM_FLOOR,
    };

    /// `ready`, `attempt_limit`, `claimed`, `claimable`, `running_past_lease`
    /// — the whole store side of the verdict, in the order the runbook's
    /// table reads them.
    fn health(
        ready_24h: i64,
        attempt_limit_24h: i64,
        claimed_24h: i64,
        claimable: i64,
        running_past_lease: i64,
    ) -> AnalysisQueueHealth {
        AnalysisQueueHealth {
            ready_24h,
            attempt_limit_24h,
            claimed_24h,
            claimable,
            running_past_lease,
            last_ready_at_ms: 0,
        }
    }

    #[test]
    fn a_queue_that_claims_and_never_finishes_is_dead() {
        assert_eq!(
            analysis_queue_verdict(health(0, 0, 25, 0, 0), 25, 25, 0),
            AnalysisQueueVerdict::Dead
        );
        // The fleet numbers that named the outage.
        assert_eq!(
            analysis_queue_verdict(health(0, 84, 396, 1_118, 11), 412, 398, 3),
            AnalysisQueueVerdict::Dead
        );
    }

    #[test]
    fn the_dead_claim_floor_is_the_boundary_it_says_it_is() {
        let just_under = ANALYSIS_DEAD_CLAIM_FLOOR - 1;
        assert_eq!(
            analysis_queue_verdict(health(0, 0, just_under, 0, 0), just_under, 0, 0),
            AnalysisQueueVerdict::Degraded
        );
        assert_eq!(
            analysis_queue_verdict(
                health(0, 0, ANALYSIS_DEAD_CLAIM_FLOOR, 0, 0),
                ANALYSIS_DEAD_CLAIM_FLOOR,
                0,
                0
            ),
            AnalysisQueueVerdict::Dead
        );
    }

    #[test]
    fn a_backlog_nobody_claims_is_dead_rather_than_idle() {
        // The shape a claim-counting rule alone misses: the queue never got
        // as far as a claim, so every counter that counts claims reads zero
        // while the work sits in front of it. Calling this `idle` is exactly
        // the silence this verdict exists to break.
        assert_eq!(
            analysis_queue_verdict(health(0, 0, 0, ANALYSIS_DEAD_BACKLOG_FLOOR, 0), 0, 0, 0),
            AnalysisQueueVerdict::Dead
        );
        assert_eq!(
            analysis_queue_verdict(health(0, 0, 0, ANALYSIS_DEAD_BACKLOG_FLOOR - 1, 0), 0, 0, 0),
            AnalysisQueueVerdict::Degraded
        );
    }

    #[test]
    fn a_queue_that_finishes_most_of_what_it_claims_is_healthy() {
        assert_eq!(
            analysis_queue_verdict(health(20, 0, 22, 4, 0), 25, 2, 0),
            AnalysisQueueVerdict::Healthy
        );
    }

    #[test]
    fn nothing_claimed_and_nothing_built_is_not_a_fault() {
        // A fully indexed library and `vod_index_mins = 0` look the same here,
        // and calling either one dead would be a false alarm every night.
        assert_eq!(
            analysis_queue_verdict(health(0, 0, 0, 0, 0), 0, 0, 0),
            AnalysisQueueVerdict::Idle
        );
        // Still idle when another pipeline is busy: `skip_markers` claims move
        // the shared lifecycle counter, and they are not evidence about this
        // queue in either direction.
        assert_eq!(
            analysis_queue_verdict(health(0, 0, 0, 0, 0), 900, 40, 0),
            AnalysisQueueVerdict::Idle
        );
    }

    #[test]
    fn a_restart_does_not_invent_a_failure_ratio() {
        // The regression this guard exists for: `attempt_limit_24h` outlives
        // the process and the old denominator did not, so any hard failure in
        // the last day made a fresh daemon report `degraded` on its first
        // sample. Both sides are the same window now.
        assert_eq!(
            analysis_queue_verdict(health(60, 1, 61, 0, 0), 0, 0, 0),
            AnalysisQueueVerdict::Healthy
        );
    }

    #[test]
    fn losing_a_quarter_of_its_claims_is_degraded_even_while_producing() {
        assert_eq!(
            analysis_queue_verdict(health(3, 0, 25, 0, 0), 25, 12, 0),
            AnalysisQueueVerdict::Degraded
        );
    }

    #[test]
    fn the_lease_loss_ratio_waits_for_a_sample_worth_dividing() {
        let under = ANALYSIS_RATIO_CLAIM_FLOOR - 1;
        // One preemption out of a handful of claims is a Tuesday, not a fault.
        assert_eq!(
            analysis_queue_verdict(health(5, 0, 6, 0, 0), under, under / 4 + 1, 0),
            AnalysisQueueVerdict::Healthy
        );
        // At the floor the same proportion is worth reporting.
        assert_eq!(
            analysis_queue_verdict(
                health(5, 0, 6, 0, 0),
                ANALYSIS_RATIO_CLAIM_FLOOR,
                ANALYSIS_RATIO_CLAIM_FLOOR / 4,
                0
            ),
            AnalysisQueueVerdict::Degraded
        );
    }

    #[test]
    fn failures_and_a_standing_stale_row_each_degrade_on_their_own() {
        // A tenth of what was claimed exhausting its budget.
        assert_eq!(
            analysis_queue_verdict(health(30, 4, 40, 0, 0), 0, 0, 0),
            AnalysisQueueVerdict::Degraded
        );
        // Just under a tenth is not.
        assert_eq!(
            analysis_queue_verdict(health(30, 4, 41, 0, 0), 0, 0, 0),
            AnalysisQueueVerdict::Healthy
        );
        // One sample with a lapsed row is a sweep that has not run yet.
        assert_eq!(
            analysis_queue_verdict(health(30, 0, 40, 0, 1), 40, 0, 1),
            AnalysisQueueVerdict::Healthy
        );
        // Two in a row is a queue that is not sweeping.
        assert_eq!(
            analysis_queue_verdict(health(30, 0, 40, 0, 1), 40, 0, 2),
            AnalysisQueueVerdict::Degraded
        );
    }

    #[test]
    fn too_few_claims_to_be_sure_is_not_healthy_either() {
        // Something was claimed and nothing finished. Not provable, not fine.
        assert_eq!(
            analysis_queue_verdict(health(0, 0, 3, 0, 0), 3, 3, 0),
            AnalysisQueueVerdict::Degraded
        );
        // And a lapsed row on its own is enough to keep it out of `idle`.
        assert_eq!(
            analysis_queue_verdict(health(0, 0, 0, 0, 1), 0, 0, 0),
            AnalysisQueueVerdict::Degraded
        );
    }
}

pub(crate) fn analysis_store_metrics(
    queue_json: &str,
    marker_json: &str,
    lifecycle_json: &str,
    health: AnalysisQueueHealth,
) -> AnalysisStoreMetrics {
    let mut metrics = AnalysisStoreMetrics {
        health,
        ..AnalysisStoreMetrics::default()
    };
    for row in serde_json::from_str::<Vec<AnalysisQueueMetricRow>>(queue_json).unwrap_or_default() {
        let Some(component) = ANALYSIS_METRIC_COMPONENTS
            .iter()
            .position(|value| *value == row.component)
        else {
            continue;
        };
        let Some(state) = ANALYSIS_METRIC_STATES
            .iter()
            .position(|value| *value == row.state)
        else {
            continue;
        };
        let Some(priority) = ANALYSIS_METRIC_PRIORITIES
            .iter()
            .position(|value| *value == row.priority)
        else {
            continue;
        };
        let Some(trigger) = ANALYSIS_METRIC_TRIGGERS
            .iter()
            .position(|value| *value == row.trigger)
        else {
            continue;
        };
        let slot = ((component * ANALYSIS_METRIC_STATES.len() + state)
            * ANALYSIS_METRIC_PRIORITIES.len()
            + priority)
            * ANALYSIS_METRIC_TRIGGERS.len()
            + trigger;
        metrics.queue_depth[slot] = row.count.max(0);
        metrics.queue_oldest_age_seconds[slot] = row.oldest.max(0);
    }
    for row in serde_json::from_str::<Vec<AnalysisMarkerMetricRow>>(marker_json).unwrap_or_default()
    {
        let Some(kind) = ANALYSIS_MARKER_KINDS
            .iter()
            .position(|value| *value == row.kind)
        else {
            continue;
        };
        let Some(provenance) = ANALYSIS_MARKER_PROVENANCE
            .iter()
            .position(|value| *value == row.provenance)
        else {
            continue;
        };
        let Some(confidence) = ANALYSIS_MARKER_CONFIDENCE
            .iter()
            .position(|value| *value == row.confidence)
        else {
            continue;
        };
        let slot = (kind * ANALYSIS_MARKER_PROVENANCE.len() + provenance)
            * ANALYSIS_MARKER_CONFIDENCE.len()
            + confidence;
        metrics.marker_counts[slot] = row.count.max(0);
    }
    for row in
        serde_json::from_str::<Vec<AnalysisLifecycleMetricRow>>(lifecycle_json).unwrap_or_default()
    {
        let Some(slot) = ANALYSIS_LIFECYCLE_METRICS
            .iter()
            .position(|(event, reason)| *event == row.event && *reason == row.reason)
        else {
            continue;
        };
        metrics.lifecycle_counts[slot] = row.count.max(0);
    }
    metrics
}

#[async_trait]
pub trait MetricsStore: Send + Sync + 'static {
    async fn prometheus_store_snapshot(
        &self,
        node_id: &str,
        now: i64,
    ) -> Result<PrometheusStoreSnapshot, StoreError>;
}

/// Replicated owner/term/generation proof attached to every provider artwork
/// mutation.
/// A timeout may drop a submitted client future without cancelling its Raft
/// command, so the database mutation itself must reject an obsolete claim.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ArtworkRepairFence {
    pub item_id: i64,
    pub owner_node_id: String,
    pub leader_term: i64,
    /// Monotonic per-item attempt generation. This distinguishes sequential
    /// repairs by the same leader, so a timed-out command from an earlier
    /// attempt cannot become valid during a later retry in the same term.
    pub generation: i64,
}

/// The text a backend may write into a durable Trakt bearer column.
///
/// Every store implementation funnels its bearer writes through here, so the
/// promise in `CLUSTERING-PLAN.md` §3.2 — a durable, and once M2 activates a
/// replicated, row carries only wrapped material — is enforced at the write
/// rather than assumed of every caller. Refusing costs one linked account a
/// loud error; accepting would put a cleartext bearer token in every voter's
/// raft log, where deleting the row cannot reach it.
pub(crate) fn persistable_credential(value: &SealedSecret) -> Result<String, StoreError> {
    value.to_persist().map(str::to_owned).map_err(|error| {
        StoreError::Credential(format!(
            "refusing to persist a Trakt bearer credential that is not sealed: {error}"
        ))
    })
}

/// Well-known settings keys. Keys are dotted, lowercase, and owned by the
/// module that writes them.
pub mod keys {
    /// Existing absolute directory for portable cluster backup artefacts.
    /// Empty or absent means the schedule performs no work.
    pub const BACKUP_DESTINATION: &str = "backup.destination";
    /// One daily UTC wall-clock minute in `HH:MM` form.
    pub const BACKUP_SCHEDULE_UTC: &str = "backup.schedule_utc";
    /// Number of complete artefact directories retained at the destination.
    pub const BACKUP_KEEP: &str = "backup.keep";
    /// Lineage marker written only by offline restore.
    pub const CLUSTER_RESTORE_GENERATION: &str = "cluster.restore_generation";
    /// Runtime Library-channel playback switch. The feature is always compiled;
    /// absence is off so an upgrade never starts scheduled playback implicitly.
    pub const LIBRARY_CHANNELS_ENABLED: &str = "library_channels.enabled";
    /// Runtime-only HDHomeRun live-TV configuration. The values are read
    /// as one snapshot and written with a generation CAS; the enable bit is
    /// deliberately absent/off on upgrade.
    pub const LIVE_TV_ENABLED: &str = "live_tv.enabled";
    pub const LIVE_TV_DEVICE_IPV4: &str = "live_tv.device_ipv4";
    pub const LIVE_TV_OWNER_NODE_ID: &str = "live_tv.owner_node_id";
    pub const LIVE_TV_MAX_SESSIONS: &str = "live_tv.max_sessions";
    /// Source-preserving Live TV ceiling. Zero/absence means Original; the
    /// legacy output height remains separate for mixed-version sessions.
    pub const LIVE_TV_MAX_OUTPUT_HEIGHT: &str = "live_tv.max_output_height";
    pub const LIVE_TV_OUTPUT_HEIGHT: &str = "live_tv.output_height";
    /// Operator-selected deinterlace cadence. This is an advisory Developer
    /// choice, not part of the tuner ownership/generation tuple.
    pub const LIVE_TV_DEINTERLACE_OUTPUT: &str = "live_tv.deinterlace_output";
    pub const LIVE_TV_CONFIG_GENERATION: &str = "live_tv.config_generation";
    /// Persisted owner-handoff safety barrier. These are internal state, not
    /// operator-editable settings: a replacement owner admits only after the
    /// named prior owner proves a drain or an administrator attests physical fencing.
    pub const LIVE_TV_TRANSITION_FROM_OWNER_NODE_ID: &str = "live_tv.transition_from_owner_node_id";
    pub const LIVE_TV_TRANSITION_DRAIN_BEFORE: &str = "live_tv.transition_drain_before";
    /// Programme-guide feed. These three are *information* settings, not tuner
    /// settings: they select a read-only source the tuner contract never
    /// depends on, so unlike the tuple above they stay editable while Live TV
    /// is enabled. An operator must be able to turn a guide on without
    /// draining every viewer. Absent is `off` — the guide is the first
    /// outbound internet call the tuner owner makes on the operator's behalf,
    /// so it never starts itself.
    pub const LIVE_TV_GUIDE_SOURCE: &str = "live_tv.guide_source";
    pub const LIVE_TV_XMLTV_URL: &str = "live_tv.xmltv_url";
    pub const LIVE_TV_GUIDE_HOURS: &str = "live_tv.guide_hours";
    /// Recording. Plain settings, deliberately not part of the Live TV
    /// generation CAS: none of them changes a tuner tuple, and riding that CAS
    /// would force an operator to disable Live TV to change a padding default.
    /// Absent is off, so an upgrade never starts writing to a disk by itself.
    pub const DVR_ENABLED: &str = "dvr.enabled";
    pub const DVR_ROOT: &str = "dvr.root";
    pub const DVR_FREE_FLOOR_GB: &str = "dvr.free_floor_gb";
    pub const DVR_TUNER_RESERVE: &str = "dvr.tuner_reserve";
    pub const DVR_PAD_START_S: &str = "dvr.pad_start_s";
    pub const DVR_PAD_END_S: &str = "dvr.pad_end_s";
    pub const DVR_REMINDER_LEAD_S: &str = "dvr.reminder_lead_s";
    /// Where a reminder and a recording's start and finish are announced.
    /// Empty is off.
    pub const DVR_WEBHOOK_URL: &str = "dvr.webhook_url";
    /// Opt in to remote media-session placement only after every committed
    /// voter is publishing the current media protocol. Absent is deliberately
    /// off so rolling upgrades keep all starts local.
    pub const CLUSTER_MEDIA_POOL_ENABLED: &str = "cluster.media_pool_enabled";
    /// Opt in to automatic expired-session takeover after the web/proxy
    /// interruption corpus passes. Kept separate from new-session placement.
    pub const CLUSTER_SESSION_TAKEOVER_ENABLED: &str = "cluster.session_takeover_enabled";
    /// Stable unique id for this logical server. Generated on first startup,
    /// immutable thereafter; in a cluster it identifies the *cluster*, not a
    /// node (REQ-HA-5: one logical identity).
    pub const INSTANCE_ID: &str = "instance.id";
    /// Human-visible name of the logical server. Configuration supplies only
    /// the first value; thereafter this replicated key is authoritative.
    pub const SERVER_NAME: &str = "server.name";
    /// "Sign-ins expire": whether a login token that goes unused for
    /// `AUTH_TOKEN_IDLE_DAYS` stops authenticating. Absent is ON — the
    /// product default — and `0` restores non-expiring tokens.
    pub const AUTH_TOKEN_EXPIRY_ENABLED: &str = "auth.token_expiry_enabled";
    /// The sliding idle window in whole days (1..=3650). Absent is 90.
    pub const AUTH_TOKEN_IDLE_DAYS: &str = "auth.token_idle_days";
    /// Unix seconds at which expiry last took effect: seeded once at startup
    /// and rewritten whenever an administrator switches expiry back on. No
    /// token's idle clock starts before it, so turning expiry on — including
    /// the default taking effect on upgrade — never signs anyone out at once.
    /// Absent means the clock has not started and nothing can expire.
    pub const AUTH_TOKEN_EXPIRY_SINCE: &str = "auth.token_expiry_since";
    /// TMDB API key (set by the admin; empty/absent disables the agent).
    pub const TMDB_API_KEY: &str = "tmdb.api_key";
    /// OMDb API key — powers review-site ratings (Rotten Tomatoes / Metacritic /
    /// IMDb), which TMDB doesn't carry. Free key from omdbapi.com.
    pub const OMDB_API_KEY: &str = "omdb.api_key";
    /// Hardware-encoder preference for transcoding: "nvenc" | "qsv" | "vaapi"
    /// | "videotoolbox" | "software" | "" (automatic).
    pub const HWACCEL: &str = "transcode.hwaccel";
    /// Requested rate-control family. Missing/`bitrate` preserves the legacy
    /// VBR path exactly; `quality` is validated against every usable encoder
    /// before an effective snapshot is published.
    pub const TRANSCODE_RATE_MODE: &str = "transcode.rate_mode";
    /// Optional integer quality override. Empty/absent means the calibrated
    /// per-family default; the value matters only when rate mode is quality.
    pub const TRANSCODE_QUALITY: &str = "transcode.quality";
    /// Trakt API application credentials (the admin creates the app at
    /// trakt.tv/oauth/applications; empty/absent disables the integration).
    pub const TRAKT_CLIENT_ID: &str = "trakt.client_id";
    pub const TRAKT_CLIENT_SECRET: &str = "trakt.client_secret";
    /// Where Curator lives, and the key to read its calendar with. Both are
    /// server-side only: plurxd proxies the call so the key never reaches a
    /// browser (plan §11.2). Unset = no coming-soon rail, which is the
    /// default and changes nothing.
    pub const MONARR_URL: &str = "monarr.url";
    pub const MONARR_API_KEY: &str = "monarr.api_key";
    /// Push watch state to Curator ("1" = on). Off by default and separate
    /// from the URL/key pair, because reading Curator's calendar and sending
    /// it your household's viewing history are very different consents.
    pub const MONARR_WATCHED_SYNC: &str = "monarr.watched_sync";
    /// Preferred default audio language (ISO 639 code, e.g. "eng").
    pub const AUDIO_LANG: &str = "playback.audio_lang";
    /// Preferred default subtitle language (ISO 639 code, e.g. "eng").
    pub const SUB_LANG: &str = "playback.sub_lang";
    /// When subtitles auto-select: "auto" (only when the audio isn't the
    /// preferred language) | "always" | "off".
    pub const SUB_MODE: &str = "playback.sub_mode";
    /// Scheduled-job intervals, in minutes; "0" or absent means off, which is
    /// the default for every one of them — upgrading a server must not
    /// silently give it new nightly habits. Per-library scan and refresh
    /// intervals live on the library row instead, since they differ per
    /// library; only the server-wide jobs are settings.
    pub const JOB_PROBE_RETRY_MINS: &str = "jobs.probe_retry_mins";
    pub const JOB_TRANSCODE_CLEANUP_MINS: &str = "jobs.transcode_cleanup_mins";
    /// Re-fetch artwork for enriched items whose poster or hero art is still
    /// incomplete.
    ///
    /// The one job that is **on by default** ([`ARTWORK_RETRY_DEFAULT_MINS`]),
    /// against the rule above and deliberately. Every other job is optional
    /// housekeeping; this one repairs a hole the server itself left, and a
    /// default of 0 would ship the bug it exists to fix — the backlog would
    /// sit there until someone found a button, which is exactly the state
    /// that made this necessary. Set it to 0 to turn it off.
    pub const JOB_ARTWORK_RETRY_MINS: &str = "jobs.artwork_retry_mins";
    /// When those last ran, unix seconds. Persisted rather than kept in memory
    /// so restarting the server doesn't restart the clock.
    pub const JOB_LAST_PROBE_RETRY: &str = "jobs.last_probe_retry";
    pub const JOB_LAST_TRANSCODE_CLEANUP: &str = "jobs.last_transcode_cleanup";
    pub const JOB_LAST_ARTWORK_RETRY: &str = "jobs.last_artwork_retry";
    /// Node-local playback telemetry retention, in days. Missing means
    /// [`TELEMETRY_RETAIN_DEFAULT_DAYS`]; `0` disables both writes and pruning.
    /// This is deliberately on by default: it is bounded local bookkeeping and
    /// the measurement referee for every Performance II milestone.
    pub const TELEMETRY_RETAIN_DAYS: &str = "telemetry.retain_days";
    /// Opt-in node-local network history used to seed Auto quality. Missing
    /// and every value other than `"1"` are off.
    pub const PLAYBACK_NETWORK_PRIORS: &str = "playback.network_priors";
    /// Opt-in web Auto controller. Missing and every value other than `"1"`
    /// are off, leaving the server's initial Auto choice in place.
    pub const PLAYBACK_AUTO_ABR: &str = "playback.auto_abr";
    /// Opt-in Android TV refresh-rate matching. Missing and every value other
    /// than `"1"` are off; readiness observations are advisory only.
    pub const PLAYBACK_DISPLAY_MODE_MATCH: &str = "playback.display_mode_match";
    /// Last successful bounded telemetry-prune pass, in unix seconds.
    pub const JOB_LAST_TELEMETRY_PRUNE: &str = "jobs.last_telemetry_prune";
    pub const TELEMETRY_RETAIN_DEFAULT_DAYS: i64 = 30;
    /// Fixed retention for the aggregate network-prior rows. The feature is
    /// opt-in and cardinality-bounded as well; this age bound prevents old
    /// networks from surviving indefinitely while it remains enabled.
    pub const NETWORK_PRIOR_RETAIN_DAYS: i64 = 30;
    /// Default artwork-retry interval, in minutes. Half-hourly: often enough
    /// that a scan interrupted by a TMDB blip repairs itself while the user is
    /// still watching that evening, rare enough to be invisible to TMDB.
    pub const ARTWORK_RETRY_DEFAULT_MINS: i64 = 30;
    /// How long an item waits between artwork attempts, in seconds. Longer
    /// than the sweep interval on purpose: the sweep decides how often to
    /// *look*, this decides how often any one item is *tried*, and a
    /// permanently art-less item should cost one request a day, not 48.
    pub const ARTWORK_RETRY_BACKOFF_SECS: i64 = 24 * 60 * 60;
    /// Arm the one-off genre backfill: "1" runs it, "done" is what the pass
    /// writes when it finishes, anything else (including absent, the default)
    /// is off.
    ///
    /// Opt-in, unlike the artwork retry, and the difference is what the two
    /// jobs cost. The artwork sweep repairs a hole the server left, from data
    /// it already has. This one re-hits the provider once per title, because
    /// nothing is stored to recompute genres from — and v9 records what
    /// happens when an upgrade decides on its own to re-fetch every library
    /// in the world. So an operator arms it, sees it finish, and it disarms
    /// itself.
    pub const GENRE_BACKFILL: &str = "genres.backfill";
    /// The highest item id the backfill has finished with, stamped after
    /// every single title.
    ///
    /// This IS the resumability. A crash, a restart or a 429 resumes at the
    /// next id instead of paying for the whole catalogue a second time, and
    /// the stamp is durable because the failure being designed against is the
    /// host rebooting mid-run — an in-memory cursor dies with the process
    /// that owns it, exactly as v10 says of retry backoff.
    pub const GENRE_BACKFILL_CURSOR: &str = "genres.backfill_cursor";
    /// Scan every library once, shortly after the server starts. "1" enables
    /// it; absent or anything else is off. For a server that was powered down
    /// while files landed — otherwise it waits out a whole interval (or, with
    /// no interval set, until someone presses a button) before noticing them.
    pub const JOB_SCAN_ON_STARTUP: &str = "jobs.scan_on_startup";
    /// How fast a remux may run, as a multiple of real time; "0" disables the
    /// limit. An unpaced remux delivers at line rate and can starve everything
    /// else sharing the link — including, over Wi-Fi, the client's own DHCP.
    /// Absent means the built-in default.
    pub const STREAM_READRATE: &str = "playback.stream_readrate";
    /// How fast an HLS session (transcode or copy-video) may read its input,
    /// as a multiple of real time; "0" disables pacing. Separate from
    /// [`STREAM_READRATE`] on purpose — the progressive remux is consumed by
    /// the browser's own back-pressure, while an HLS session writes to disk
    /// and needs its own answer.
    pub const HLS_READRATE: &str = "playback.hls_readrate";
    /// Seconds of content an HLS session may deliver flat-out before
    /// [`HLS_READRATE`] engages. This is the buffer the viewer starts with, so
    /// it is also what a marginal link gets to spend before it stalls.
    pub const HLS_BURST_SECS: &str = "playback.hls_burst_secs";
    /// Seconds of content an HLS session may write beyond the client's
    /// playhead before it is suspended; "0" lets it run unbounded. This is the
    /// disk bound that realtime pacing used to provide, minus the part where
    /// realtime pacing also prevented the viewer from ever building a buffer.
    pub const HLS_AHEAD_MAX_SECS: &str = "playback.hls_ahead_max_secs";
    /// The same window in bytes, per session. Time alone is not a disk
    /// contract: 180 seconds is a few hundred megabytes at a transcode rung
    /// and over a gigabyte of 4K copy, so a stream with an unexpectedly high
    /// bitrate would blow through any time-only limit.
    pub const HLS_AHEAD_MAX_BYTES: &str = "playback.hls_ahead_max_bytes";
    /// Ceiling on scratch across every live session. A per-session cap bounds
    /// one runaway session; it says nothing about several healthy ones filling
    /// the disk between them.
    pub const HLS_SCRATCH_MAX_BYTES: &str = "playback.hls_scratch_max_bytes";
    /// Legacy-peer playlist setting, retained for rolling upgrades. Current
    /// rolling sessions always serve a typeless sliding playlist from their
    /// first response; this value cannot change their presentation contract.
    pub const HLS_TYPELESS_SLIDING: &str = "playback.hls_typeless_sliding";
    /// How often, in minutes, to build fragment indexes for files that have
    /// none. Absent is every 15 minutes — the default the settings API
    /// reports — and `0` is an explicit pause.
    ///
    /// Nothing reads an index yet; a file without one keeps today's
    /// presentation, so this job is invisible to every client either way.
    pub const VOD_INDEX_MINS: &str = "playback.vod_index_mins";
    /// Content-addressed cluster coordination for VOD indexes. Missing/zero is
    /// off so an upgrade never starts full-library reads without the operator's
    /// topology measurement and explicit opt-in.
    pub const VOD_INDEX_CLUSTER_CACHE: &str = "playback.vod_index_cluster_cache";
    /// Durable analysis retry budget. The settings API constrains this to a
    /// small positive range so an operator can tune slow media without making
    /// a corrupt source retry forever.
    pub const ANALYSIS_MAX_ATTEMPTS: &str = "analysis.max_attempts";
    /// Claim lease and capped exponential retry policy, in seconds.
    pub const ANALYSIS_LEASE_SECS: &str = "analysis.lease_secs";
    pub const ANALYSIS_BACKOFF_BASE_SECS: &str = "analysis.backoff_base_secs";
    pub const ANALYSIS_BACKOFF_MAX_SECS: &str = "analysis.backoff_max_secs";
    /// Forward subtitle materialization span. The settings API constrains this
    /// to 30–900 seconds and absent means the 200-second default.
    pub const SUBTITLE_WINDOW_SECS: &str = "playback.subtitle_window_secs";
    /// Answer a subtitle segment whose sidecar has failed with `503` +
    /// `Retry-After`, instead of a syntactically valid but empty track.
    ///
    /// Off by default, and the reason is measurement rather than caution:
    /// AVPlayer gives a subtitle segment roughly two seconds and blocks the
    /// muxed video while it waits, so how each engine reacts to a refusal on
    /// that request — keeps playing video, or stalls the picture — has to be
    /// observed per engine before this can flip. The Developer tab reports
    /// what has been observed, advisory only; it never blocks the switch.
    pub const SUBTITLE_NOT_READY_503: &str = "playback.subtitle_not_ready_503";
    /// Let the two PGS consumers — the overlay's stage and the burn sidecar —
    /// read a track the subtitle-source store kept, instead of demuxing the
    /// whole source. On when absent. Off makes both ignore the store entirely,
    /// so a wrong artifact a producer published is taken out of service with
    /// one switch and no redeploy.
    pub const SUBTITLE_STORED_SOURCES: &str = "subtitles.stored_sources";
    pub const SUBTITLE_CLUSTER_SOURCES: &str = "subtitles.cluster_sources";
    pub const SUBTITLE_BACKFILL: &str = "subtitles.backfill";
    /// Make a chapter thumbnail on request and keep it in the runtime
    /// cache. On by default; off answers the route 404 and extracts nothing.
    pub const CHAPTER_THUMBNAILS: &str = "playback.chapter_thumbnails";
    /// VOD availability kill switch. Absent/on accepts immutable VOD session
    /// creation; `0` refuses it. It never selects the removed live HLS path.
    pub const VOD_PRESENTATION: &str = "playback.vod_presentation";
    /// Temporary availability guard while immutable-VOD prerequisites are
    /// backfilled. Absent/on lets a typed VOD prerequisite refusal use the
    /// retained growing-HLS engine; `0` makes the VOD refusal final again.
    pub const VOD_LIVE_RECOVERY: &str = "playback.vod_live_recovery";
    /// Advertise the additive v1 playback-control endpoint on newly created
    /// HLS sessions. Absent/on advertises it now that every client has a
    /// reporter and prepared-switch adapter; explicit `0` remains an operator
    /// override.
    pub const PLAYBACK_CONTROL_PROTOCOL_V1: &str = "playback.control_protocol_v1";
    /// Start and prime prepared quality successors. Absent/on enables the
    /// shipped transaction; explicit `0` is the live operator override.
    pub const PREPARED_QUALITY_HANDOFF: &str = "playback.prepared_quality_handoff";
    /// Permit automatic hardware-to-software decoder recovery for new
    /// producer attempts. This is an operator switch, not a qualification
    /// request: missing diagnostic contracts are reported as advisory facts
    /// and never override an explicit enable.
    pub const AUTOMATIC_DECODER_RECOVERY: &str = "playback.automatic_decoder_recovery";
    /// Ask this node to plan into the health-qualified artifact identity, so a
    /// transcode may only be reused when its producer's own receipt says the
    /// decode was clean.
    ///
    /// A request, not a switch. It names a *different content-addressed key
    /// space*, so a node that turned it on without being able to honour it
    /// would rotate its whole transcode cache and then refuse every generation
    /// it produced. The node intersects this with what it has actually
    /// measured — a diagnostic contract covering its own FFmpeg build, and a
    /// decoder inventory that names implementations that contract was
    /// qualified against — and stays unqualified with a stated reason when it
    /// cannot honour the request. Absent or `0` is off.
    pub const DECODER_HEALTH_QUALIFIED_ARTIFACTS: &str =
        "playback.decoder_health_qualified_artifacts";
    /// Serve PGS subtitle tracks through the authenticated `pgs-v1` overlay
    /// API instead of hiding them.
    ///
    /// This was `PLURX_PGS_OVERLAY`, read once at boot. An environment
    /// variable is the wrong place for it twice over: it decides which
    /// subtitle tracks a client is even offered, which is a product question
    /// an operator should be able to see and answer, and it could only be
    /// changed by restarting the daemon with a different compose file. The
    /// setting is the switch now; the old variable is read once at startup to
    /// seed it, so a deployment that had turned it on keeps it on and can
    /// then find it in Settings.
    pub const PGS_OVERLAY: &str = "subtitles.pgs_overlay";
    /// Convert a Dolby Vision Profile 7 title to Profile 8.1 so a Dolby Vision
    /// client sees Dolby Vision rather than HDR10.
    ///
    /// This was `PLURX_DV_CONVERT`, and unlike most switches it defaults *on*:
    /// the conversion is plurx's own code, and a Profile 7 title reaching a
    /// Dolby Vision client as HDR10 is the thing it exists to stop. Absent
    /// therefore means on. It is a setting rather than an environment variable
    /// for the same reason as the overlay above: an operator turning off work
    /// their GPU is doing should be able to find the switch, and see it is off.
    pub const DV_CONVERT: &str = "playback.dolby_vision_convert";
    /// Node-wide byte budget for un-admitted VOD rendition working sets.
    /// Absent takes the built-in default. A parsed zero is refused at the
    /// settings surface: "no working set" and "not configured" are opposite
    /// answers and only the caller knows which one was meant (M3 handoff §6).
    pub const VOD_WORKING_SET_BYTES: &str = "playback.vod_working_set_bytes";
    /// Server ceiling, in seconds, for one blocking VOD segment fetch. The
    /// per-request `block_budget_secs` is clamped to this.
    pub const VOD_BLOCK_BUDGET_SECS: &str = "playback.vod_block_budget_secs";
    /// Hard producer deadline from the first blocked request for a planned
    /// segment until bytes or a typed `producer_failed` answer. Distinct from
    /// the shorter per-request block deadline so clients can retry 503s while
    /// the same bounded production attempt continues.
    pub const VOD_MATERIALIZE_BUDGET_SECS: &str = "playback.vod_materialize_budget_secs";
    /// How many blocked VOD segment GETs this node will hold at once.
    ///
    /// A blocked GET is a parked request holding an admission slot and a
    /// retention pin until its segment lands, so the cap is what stops a seek
    /// storm turning into unbounded parked work. Promised as a setting by the
    /// VOD presentation plan §2.3 and hard-coded until now; the per-session
    /// cap stays fixed because it bounds one viewer rather than the node.
    ///
    /// The right number is a property of the deployment, not of the code: a
    /// node serving a handful of viewers wants far less parked work than one
    /// fronting a household, and an operator watching `pool_full` refusals is
    /// the only one who can tell which they have.
    pub const VOD_BLOCKED_GET_CAP: &str = "playback.vod_blocked_get_cap";
    /// How many transcodes may run on the hardware encoder at once.
    ///
    /// An iGPU has one video-processing block, and two 4K sessions on it do not
    /// run at half speed each — they contend, and both can fall under realtime.
    /// One person's stream becomes two people's stutter. Settable because the
    /// right number is a property of the silicon: a discrete card with two
    /// NVENC chips is not a NUC.
    pub const MAX_HW_SESSIONS: &str = "transcode.max_hw_sessions";
    /// Threads the software-encoder CPU pool may hand out at once. Defaults
    /// to every core but one (plurxd derives it from the machine); an admin
    /// sets it lower on a box whose CPU has other jobs, or higher at their
    /// own risk.
    ///
    /// A dropdown in Playback → Streaming, alongside [`MAX_HW_SESSIONS`]. Note
    /// what a stored `0` means here, because it is not what it looks like:
    /// `SwPool::try_take` grants unconditionally while the pool is empty, so a
    /// zero budget is not a ban but a degradation to one saturating session at
    /// a time. The default is per-node while this key is replicated, so a
    /// write pins one machine's core count on the whole cluster — which is why
    /// the page sends this field only when an operator actually moved it.
    pub const SW_POOL_THREADS: &str = "transcode.software_pool_threads";
    /// Disk the pre-transcode cache may occupy, in gigabytes. `0` turns the
    /// cache off: nothing is produced, and what is already there is evicted.
    ///
    /// A budget rather than a target. The cache is worth exactly what it
    /// saves, and a server that filled its disk to spare a few seconds has
    /// made the trade backwards. Eviction is LRU, so what survives is what
    /// people actually come back to.
    pub const CACHE_MAX_GB: &str = "cache.max_gb";
    /// Last user id inspected by the bounded speculative-candidate fan-out.
    /// The singleton lease makes advancing this replicated cursor race-free.
    pub const CACHE_PRETRANSCODE_USER_CURSOR: &str = "cache.pretranscode_user_cursor";
    /// Master kill switch for app-managed offline packages. Missing means on;
    /// an operator can stop new admission without invalidating local copies.
    pub const OFFLINE_ENABLED: &str = "offline.enabled";
    /// Global and per-user reservations for pinned offline artifacts. These
    /// are separate from the playback cache so a flight queue cannot evict
    /// the bytes an active viewer is reading.
    pub const OFFLINE_MAX_GB: &str = "offline.max_gb";
    pub const OFFLINE_MAX_GB_PER_USER: &str = "offline.max_gb_per_user";
    /// Registry hygiene, not the primary quota (bytes are). Includes failed
    /// rows until their bounded diagnostic retention sweep removes them.
    pub const OFFLINE_MAX_ROWS_PER_USER: &str = "offline.max_rows_per_user";
    /// How often the producer looks for something worth pre-transcoding, in
    /// minutes; "0" is off, and off is the default like every other job — an
    /// upgraded server must not start encoding overnight on its own.
    pub const JOB_CACHE_PRODUCE_MINS: &str = "jobs.cache_produce_mins";
    pub const JOB_LAST_CACHE_PRODUCE: &str = "jobs.last_cache_produce";
    /// Node-local, like the transcode-cleanup stamp: an index lives on the
    /// node that built it, so when it last ran is a fact about that node.
    pub const JOB_LAST_VOD_INDEX: &str = "jobs.last_vod_index";
    /// Last file examined by this node's bounded VOD index walk. Without a
    /// cursor, one slow or malformed title at the front of a library consumes
    /// every pass and later files can never become playable.
    pub const JOB_VOD_INDEX_CURSOR: &str = "jobs.vod_index_cursor";
    /// Set once the Dolby Vision columns have been filled in from stored probe
    /// JSON for every file that had any. The incremental scanner skips
    /// unchanged files, so without a backfill an existing library would never
    /// gain the columns short of a destructive re-add.
    pub const JOB_DV_BACKFILL_DONE: &str = "jobs.dv_facts_backfilled";
    /// How far this node's Dolby Vision backfill has walked. Node-local, and
    /// strictly-after: a row this pass cannot fix — a Dolby Vision codec tag
    /// whose stored probe has no configuration record — would otherwise sit at
    /// the front of every window forever, hiding every fixable row behind it.
    pub const JOB_DV_BACKFILL_CURSOR: &str = "jobs.dv_facts_backfill_cursor";
    /// Set after every stored probe row has been considered for the additive
    /// sample-entry column. Unknown or malformed labels remain null by design.
    pub const JOB_VIDEO_CODEC_TAG_BACKFILL_DONE: &str = "jobs.video_codec_tag_backfilled";
    /// Node-local strictly-after cursor for the bounded sample-entry walk.
    pub const JOB_VIDEO_CODEC_TAG_BACKFILL_CURSOR: &str = "jobs.video_codec_tag_backfill_cursor";
    /// Set after the bounded stored-probe walk has assigned every pre-column
    /// file either its reporter token or the explicit `unknown` value.
    pub const JOB_FIELD_ORDER_BACKFILL_DONE: &str = "jobs.field_order_backfilled";
    /// Node-local strictly-after cursor for the field-order backfill.
    pub const JOB_FIELD_ORDER_BACKFILL_CURSOR: &str = "jobs.field_order_backfill_cursor";
    pub const JOB_LUMINANCE_BACKFILL_DONE: &str = "jobs.luminance_backfilled";
    pub const JOB_LUMINANCE_BACKFILL_CURSOR: &str = "jobs.luminance_backfill_cursor";
    /// Per-library permanent Profile 7 conversion policy, encoded as a JSON
    /// object from decimal library id to `off`, `manual`, or `auto`. Missing
    /// libraries are always off: an upgrade must never rewrite media by
    /// surprise.
    pub const LIBRARY_DV_DISK_CONVERT: &str = "library.dv_disk_convert";
    /// Whether a verified source is retained as `<source>.p7.orig`. Missing is
    /// on, because the safe default preserves the operator's original bytes.
    pub const LIBRARY_DV_DISK_KEEP_ORIGINAL: &str = "library.dv_disk_keep_original";
    /// Cluster-wide conversion slots. Missing or invalid is one.
    pub const LIBRARY_DV_DISK_CONVERT_PARALLEL: &str = "library.dv_disk_convert_parallel";
    /// Highest file id examined by this node's bounded queue walk.
    pub const JOB_DV_DISK_CONVERT_CURSOR: &str = "jobs.dv_disk_convert_cursor";
    /// Last auto-enabled library examined by this node. Discovery itself is
    /// cluster-leased, but the cursor keeps a repeatedly busy library from
    /// hiding later libraries when each tick admits only one bounded batch.
    pub const JOB_DV_DISK_AUTO_LIBRARY_CURSOR: &str = "jobs.dv_disk_auto_library_cursor";
}

#[async_trait]
pub trait SettingsStore: Send + Sync + 'static {
    /// Cheap liveness probe of the backing storage (drives `/readyz`).
    async fn ping(&self) -> Result<(), StoreError>;
    async fn get_setting(&self, key: &str) -> Result<Option<String>, StoreError>;
    /// Atomically seed an absent setting and return the durable winner.
    async fn get_or_init_setting(&self, key: &str, seed: &str) -> Result<String, StoreError>;
    /// Read two related settings from one database snapshot.
    async fn get_setting_pair(
        &self,
        first: &str,
        second: &str,
    ) -> Result<(Option<String>, Option<String>), StoreError>;
    /// Read the complete settings table from one database snapshot.
    ///
    /// Administrative views render many independent settings at once. A
    /// single snapshot keeps those values mutually consistent and, on a
    /// replicated backend, avoids paying one linearizable read per field.
    async fn settings_snapshot(&self) -> Result<BTreeMap<String, String>, StoreError>;
    async fn put_setting(&self, key: &str, value: &str) -> Result<(), StoreError>;
    /// Insert an immutable setting value. Returns false when the key already
    /// exists and leaves the first committed value unchanged.
    async fn put_setting_if_absent(&self, key: &str, value: &str) -> Result<bool, StoreError>;
    /// Insert an immutable setting only while the named provider-repair claim
    /// is still current in the same replicated transaction.
    async fn put_setting_if_absent_if_artwork_repair_current(
        &self,
        key: &str,
        value: &str,
        expected_item_id: i64,
        fence: &ArtworkRepairFence,
    ) -> Result<bool, StoreError>;
    /// Remove immutable Curator origin records for one artwork generation,
    /// but only when no catalogue row references that filename in the same
    /// transaction. This bounds replicated history after orphan collection.
    async fn prune_unreferenced_book_cover_origins(
        &self,
        filename: &str,
    ) -> Result<usize, StoreError>;
    /// Atomically publish a related group of settings.
    ///
    /// Callers use this when one key activates the meaning of another. A
    /// partial write must never leave a durable configuration that no request
    /// actually submitted.
    async fn put_settings(&self, values: &[(&str, &str)]) -> Result<(), StoreError>;
    /// Atomically replace a related settings tuple only while its generation
    /// still equals `expected_generation`. `values` must include
    /// `generation_key` set to exactly expected + 1. A false result is a
    /// normal concurrent-writer conflict and leaves every value unchanged.
    async fn put_settings_if_generation(
        &self,
        generation_key: &str,
        expected_generation: i64,
        values: &[(&str, &str)],
    ) -> Result<bool, StoreError>;
    /// The stable unique id of this logical server.
    async fn instance_id(&self) -> Result<String, StoreError>;
}

pub(crate) fn validate_generated_settings(
    generation_key: &str,
    expected_generation: i64,
    values: &[(&str, &str)],
) -> Result<(), StoreError> {
    let next = expected_generation
        .checked_add(1)
        .ok_or_else(|| StoreError::Database("settings generation exhausted".to_owned()))?;
    let matches = values
        .iter()
        .filter(|(key, value)| *key == generation_key && value.parse::<i64>().ok() == Some(next))
        .count();
    let unique = values
        .iter()
        .map(|(key, _)| *key)
        .collect::<std::collections::BTreeSet<_>>()
        .len()
        == values.len();
    if expected_generation < 0 || matches != 1 || !unique {
        return Err(StoreError::Database(
            "generated settings require unique keys and exactly the next generation".to_owned(),
        ));
    }
    Ok(())
}

#[async_trait]
pub trait UserStore: Send + Sync + 'static {
    async fn count_users(&self) -> Result<i64, StoreError>;
    async fn create_user(
        &self,
        username: &str,
        password_hash: &str,
        is_admin: bool,
    ) -> Result<User, StoreError>;
    async fn get_user(&self, id: i64) -> Result<Option<User>, StoreError>;
    async fn get_user_by_username(&self, username: &str) -> Result<Option<User>, StoreError>;
    async fn list_users(&self) -> Result<Vec<User>, StoreError>;
    /// Deterministic bounded page for background fan-out. Interactive admin
    /// lists keep using `list_users`; schedulers must not allocate every user
    /// merely to inspect a small prediction rail.
    async fn list_users_page(&self, after_id: i64, limit: i64) -> Result<Vec<User>, StoreError>;
    async fn delete_user(&self, id: i64) -> Result<bool, StoreError>;
    /// Delete a user only when doing so leaves at least one administrator.
    /// The predicate and delete must be one Store mutation so concurrent admin
    /// requests cannot both pass a separate count preflight.
    async fn delete_user_preserving_admin(
        &self,
        id: i64,
        claim: Option<&CacheAdminMutationClaim>,
    ) -> Result<bool, StoreError>;
    async fn count_admins(&self) -> Result<i64, StoreError>;
    /// Replace a user's password hash. Callers should also revoke the user's
    /// tokens so old sessions die with the old password.
    async fn set_password(&self, id: i64, password_hash: &str) -> Result<bool, StoreError>;
    /// Atomically replace a password and revoke every existing login token.
    async fn reset_password_and_revoke_tokens(
        &self,
        id: i64,
        password_hash: &str,
        claim: Option<&CacheAdminMutationClaim>,
    ) -> Result<bool, StoreError>;
    /// Atomically grant administrator status, replace the password, and revoke
    /// every existing token. A failed combined update must never leave an old
    /// session newly authorized as an administrator.
    async fn promote_user_and_reset_password(
        &self,
        id: i64,
        password_hash: &str,
        claim: Option<&CacheAdminMutationClaim>,
    ) -> Result<bool, StoreError>;
    async fn set_admin(&self, id: i64, is_admin: bool) -> Result<bool, StoreError>;
    /// Revoke admin status only when another administrator exists in the same
    /// Store mutation.
    async fn demote_user_preserving_admin(
        &self,
        id: i64,
        claim: Option<&CacheAdminMutationClaim>,
    ) -> Result<bool, StoreError>;
    /// Revoke every login token for one user; returns how many were dropped.
    async fn delete_tokens_for_user(&self, user_id: i64) -> Result<u64, StoreError>;

    /// Register a login token. Only the SHA-256 hash of the token is stored.
    async fn create_token(
        &self,
        token_hash: &str,
        user_id: i64,
        device: Option<&str>,
    ) -> Result<(), StoreError>;
    /// Register a login token only if the user's password is still the exact
    /// version the caller authenticated. This closes the interval in which a
    /// concurrent reset could otherwise finish before token insertion.
    async fn create_token_if_password_matches(
        &self,
        token_hash: &str,
        user_id: i64,
        device: Option<&str>,
        expected_password_hash: &str,
    ) -> Result<bool, StoreError>;
    /// Resolve a token hash under the server's sign-in expiry policy, read in
    /// the same snapshot as the token row. A live token's coalesced
    /// `last_seen_at` is refreshed exactly as before; an expired one is
    /// reported and left untouched.
    async fn authenticate_token(&self, token_hash: &str)
        -> Result<TokenAuthentication, StoreError>;
    /// Resolve a token hash to its user (touching `last_seen_at`). An expired
    /// token resolves to nobody, so every caller that predates expiry — the
    /// Plex facade, recovery reads — honours the policy without knowing it.
    async fn user_for_token(&self, token_hash: &str) -> Result<Option<User>, StoreError> {
        Ok(match self.authenticate_token(token_hash).await? {
            TokenAuthentication::Authenticated(user) => Some(user),
            TokenAuthentication::Expired { .. } | TokenAuthentication::Unknown => None,
        })
    }
    async fn delete_token(&self, token_hash: &str) -> Result<bool, StoreError>;
    /// Delete a login token only while the exact clustered cache-revocation
    /// exclusion claim is still live. Standalone SQLite passes no claim.
    async fn delete_token_with_cache_admin_claim(
        &self,
        token_hash: &str,
        claim: Option<&CacheAdminMutationClaim>,
    ) -> Result<bool, StoreError>;
    /// List a bounded, oldest-first inventory without exposing full token
    /// digests to the HTTP layer.
    async fn list_tokens_for_user(&self, user_id: i64) -> Result<Vec<TokenSummary>, StoreError>;
    /// Delete exactly one token selected by an eight-hex prefix. Ambiguous
    /// prefixes are never accepted, and clustered writes retain the exact
    /// cache-admin mutation claim.
    async fn delete_token_by_prefix_for_user(
        &self,
        user_id: i64,
        prefix: &str,
        claim: Option<&CacheAdminMutationClaim>,
    ) -> Result<DeleteTokenByPrefixOutcome, StoreError>;
}

#[async_trait]
pub trait LibraryStore: Send + Sync + 'static {
    async fn create_library(&self, library: &NewLibrary) -> Result<Library, StoreError>;
    async fn update_library(
        &self,
        id: i64,
        library: &NewLibrary,
    ) -> Result<Option<Library>, StoreError>;
    async fn delete_library(&self, id: i64) -> Result<bool, StoreError>;
    /// Set the automatic scan/refresh intervals (minutes; `0` = off). Separate
    /// from `update_library` because the schedule is not part of a library's
    /// identity — the settings UI edits one without touching the other, and a
    /// path edit must never silently reset a schedule.
    async fn set_library_schedule(
        &self,
        id: i64,
        scan_interval_mins: i64,
        refresh_interval_mins: i64,
    ) -> Result<Option<Library>, StoreError>;
    /// Stamp a completed run. `refreshed` also stamps the refresh clock, since
    /// a refresh does everything a scan does.
    async fn mark_library_scanned(&self, id: i64, refreshed: bool) -> Result<(), StoreError>;
    async fn get_library(&self, id: i64) -> Result<Option<Library>, StoreError>;
    async fn list_libraries(&self) -> Result<Vec<Library>, StoreError>;
}

#[async_trait]
pub trait LibraryChannelStore: Send + Sync + 'static {
    async fn subject_job(
        &self,
        query: crate::channel_subjects::JobQuery,
    ) -> Result<Option<crate::channel_subjects::SubjectJob>, StoreError>;
    async fn subject_write(
        &self,
        write: crate::channel_subjects::JobWrite,
    ) -> Result<bool, StoreError>;
    async fn subject_decisions(
        &self,
        owner: i64,
        subject: &str,
        profile: &str,
        ids: &[(i64, String)],
    ) -> Result<Vec<crate::channel_subjects::CachedDecision>, StoreError>;

    async fn list_library_channels(
        &self,
        actor_user_id: i64,
        actor_is_admin: bool,
        management: bool,
        after_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<crate::library_channels::LibraryChannel>, StoreError>;

    async fn get_library_channel(
        &self,
        actor_user_id: i64,
        actor_is_admin: bool,
        channel_id: &str,
    ) -> Result<Option<crate::library_channels::LibraryChannel>, StoreError>;

    async fn create_library_channel(
        &self,
        actor_is_admin: bool,
        channel: &crate::library_channels::NewLibraryChannel,
    ) -> Result<
        crate::library_channels::ChannelMutation<crate::library_channels::LibraryChannel>,
        StoreError,
    >;

    async fn update_library_channel(
        &self,
        update: &crate::library_channels::LibraryChannelUpdate,
    ) -> Result<
        crate::library_channels::ChannelMutation<crate::library_channels::LibraryChannel>,
        StoreError,
    >;

    async fn delete_library_channel(
        &self,
        deletion: &crate::library_channels::LibraryChannelDelete,
    ) -> Result<crate::library_channels::ChannelMutation<()>, StoreError>;

    async fn set_library_channel_favourite(
        &self,
        user_id: i64,
        channel_id: &str,
        favourite: bool,
        now_ms: i64,
    ) -> Result<bool, StoreError>;

    /// Read one bounded, coherent catalogue selection snapshot. Implementations
    /// execute the item/file/ancestry projection as a single database query so
    /// a channel build cannot combine pages from different catalogue states.
    async fn library_channel_catalog_snapshot(
        &self,
        limit: i64,
    ) -> Result<Vec<crate::library_channels::ChannelCandidate>, StoreError>;

    async fn list_library_channel_refresh_candidates(
        &self,
        after_id: Option<&str>,
        limit: i64,
        now_ms: i64,
    ) -> Result<Vec<crate::library_channels::LibraryChannel>, StoreError>;

    async fn prune_library_channel_state(&self, now_ms: i64, limit: i64)
        -> Result<i64, StoreError>;

    async fn claim_library_channel_build(
        &self,
        claim: &crate::library_channels::LibraryChannelBuildClaim,
    ) -> Result<crate::library_channels::ChannelBuildMutation, StoreError>;

    async fn renew_library_channel_build(
        &self,
        channel_id: &str,
        generation_id: &str,
        claim_id: &str,
        now_ms: i64,
        expires_at_ms: i64,
    ) -> Result<crate::library_channels::ChannelBuildMutation, StoreError>;

    async fn stage_library_channel_entries(
        &self,
        channel_id: &str,
        generation_id: &str,
        claim_id: &str,
        entries: &[crate::library_channels::ChannelGenerationEntry],
        now_ms: i64,
    ) -> Result<crate::library_channels::ChannelBuildMutation, StoreError>;

    async fn publish_library_channel_generation(
        &self,
        publication: &crate::library_channels::LibraryChannelPublication,
    ) -> Result<crate::library_channels::ChannelBuildMutation, StoreError>;

    async fn fail_library_channel_build(
        &self,
        failure: &crate::library_channels::LibraryChannelBuildFailure,
    ) -> Result<crate::library_channels::ChannelBuildMutation, StoreError>;

    async fn complete_library_channel_build_without_publication(
        &self,
        channel_id: &str,
        expected_revision: i64,
        candidate_count: i64,
        automatic: bool,
        now_ms: i64,
    ) -> Result<crate::library_channels::ChannelBuildMutation, StoreError>;

    async fn read_library_channel_generation(
        &self,
        actor_user_id: i64,
        actor_is_admin: bool,
        channel_id: &str,
        generation_id: &str,
    ) -> Result<Option<crate::library_channels::LibraryChannelGeneration>, StoreError>;
}

/// Recording rules, the airings they and people schedule, and reminders.
///
/// Three habits run through every method here, and each one exists because of
/// a way the obvious alternative fails:
///
/// - **Inserting an airing never updates one.** `insert_dvr_airing_if_absent`
///   reports what it found instead, so a rule that keeps matching an airing
///   the viewer skipped cannot resurrect it fifteen seconds later.
/// - **Writes that the owner loop makes are conditional on the state they
///   expect and on the tuner generation they were planned under.** A settings
///   change mid-tick must land no rows from the configuration it replaced.
/// - **Column sets do not overlap.** Progress touches two columns, a stop
///   request touches two others, and a state transition touches neither set —
///   so a 30-second progress write racing a viewer's Stop cannot erase it.
#[async_trait]
pub trait DvrStore: Send + Sync + 'static {
    async fn list_dvr_rules(&self) -> Result<Vec<crate::dvr::DvrRule>, StoreError>;

    async fn get_dvr_rule(&self, id: &str) -> Result<Option<crate::dvr::DvrRule>, StoreError>;

    /// Upsert by id. Refuses past `DVR_RULES_MAX`, reported as `false`.
    async fn put_dvr_rule(&self, rule: &crate::dvr::DvrRule) -> Result<bool, StoreError>;

    async fn delete_dvr_rule(&self, id: &str) -> Result<bool, StoreError>;

    /// Renumber every rule into the given order. A total order is what makes
    /// the scheduler's tuner allocation deterministic across voters.
    async fn reorder_dvr_rules(
        &self,
        ids_in_priority_order: &[String],
        now_ms: i64,
    ) -> Result<bool, StoreError>;

    /// Materialise one airing if `(channel_id, airing_start)` is not already
    /// spoken for. Never updates: the existing row's state comes back instead.
    async fn insert_dvr_airing_if_absent(
        &self,
        row: &crate::dvr::DvrRecording,
    ) -> Result<crate::dvr::DvrInsertOutcome, StoreError>;

    /// Event-aware form of [`Self::insert_dvr_airing_if_absent`]. The event is
    /// appended only when this transaction inserts the airing.
    async fn insert_dvr_airing_with_event(
        &self,
        row: &crate::dvr::DvrRecording,
        event: &crate::dvr::DvrEventInput,
    ) -> Result<crate::dvr::DvrInsertOutcome, StoreError>;

    async fn get_dvr_recording(
        &self,
        id: &str,
    ) -> Result<Option<crate::dvr::DvrRecording>, StoreError>;

    /// The row that owns this airing, by the identity the unique index keeps.
    ///
    /// Needed because "one airing is one row" is only useful if the row can be
    /// found that way. Scanning a page of the list instead would answer "no
    /// such airing" the moment a server holds more recordings than a page.
    async fn get_dvr_recording_for_airing(
        &self,
        channel_id: &str,
        airing_start: i64,
    ) -> Result<Option<crate::dvr::DvrRecording>, StoreError>;

    /// One page, newest airing first.
    ///
    /// `after` is the cursor a previous page's last row produced
    /// ([`crate::dvr::recording_cursor`]) — `(airing_start, id)`, not an id
    /// alone, because ordering by a v4 UUID would hand a viewer an arbitrary
    /// hundred of their recordings and call it their library.
    async fn list_dvr_recordings(
        &self,
        filter: &crate::dvr::DvrRecordingFilter,
        after: Option<&str>,
        limit: i64,
    ) -> Result<Vec<crate::dvr::DvrRecording>, StoreError>;

    /// Every row in one of these states, ordered by `capture_start`. What the
    /// owner loop reads at the top of a tick.
    async fn list_dvr_recordings_in(
        &self,
        states: &[crate::dvr::DvrState],
    ) -> Result<Vec<crate::dvr::DvrRecording>, StoreError>;

    /// The bounded durable half of the foreground overview: rows that should
    /// already be capturing, their exact total, and the next future start.
    async fn dvr_overview_rows(
        &self,
        now_s: i64,
        limit: i64,
    ) -> Result<(Vec<crate::dvr::DvrRecording>, i64, Option<i64>), StoreError>;

    /// One ascending, bounded schedule window plus its exact conflict total.
    async fn list_dvr_schedule_window(
        &self,
        states: &[crate::dvr::DvrState],
        from_s: i64,
        to_s: i64,
        channel_ids: &[String],
        after: Option<(i64, &str)>,
        limit: i64,
    ) -> Result<(Vec<crate::dvr::DvrRecording>, i64), StoreError>;

    /// Applies only when the row is in one of `from` and, when
    /// `fence_generation` is given, the tuner configuration is still at that
    /// generation. Returns `false` otherwise, having written nothing.
    async fn transition_dvr_recording(
        &self,
        transition: &crate::dvr::DvrTransition<'_>,
    ) -> Result<bool, StoreError>;

    /// Apply a conditional transition and allocate its event sequence in one
    /// transaction. A rejected transition writes no event.
    async fn transition_dvr_recording_with_event(
        &self,
        transition: &crate::dvr::DvrTransition<'_>,
        event: &crate::dvr::DvrEventInput,
    ) -> Result<bool, StoreError>;

    /// Writes `bytes` and `last_progress_ms`, and nothing else, ever.
    async fn progress_dvr_recording(
        &self,
        id: &str,
        bytes: i64,
        at_ms: i64,
    ) -> Result<(), StoreError>;

    /// Record a viewer's Stop. Succeeds once on a `recording` row and is
    /// idempotent thereafter; the owner's next tick consumes it. Returns the
    /// row as it now stands, or `None` when there is no such recording.
    async fn request_dvr_stop(
        &self,
        id: &str,
        at_ms: i64,
        by_user: i64,
    ) -> Result<Option<crate::dvr::DvrRecording>, StoreError>;

    async fn request_dvr_stop_with_event(
        &self,
        id: &str,
        at_ms: i64,
        by_user: i64,
        event: &crate::dvr::DvrEventInput,
    ) -> Result<Option<crate::dvr::DvrRecording>, StoreError>;

    /// Re-point pending rule rows at the rule that now owns them.
    async fn repoint_dvr_rule_rows(
        &self,
        changes: &[(String, Option<String>)],
        now_ms: i64,
    ) -> Result<(), StoreError>;

    /// What the scan writes back once a finished capture is in the library.
    async fn link_dvr_recording_media(
        &self,
        recording_id: &str,
        item_id: i64,
        file_id: i64,
        now_ms: i64,
    ) -> Result<bool, StoreError>;

    async fn link_dvr_recording_media_with_event(
        &self,
        recording_id: &str,
        item_id: i64,
        file_id: i64,
        now_ms: i64,
        event: &crate::dvr::DvrEventInput,
    ) -> Result<bool, StoreError>;

    /// Append an owner observation once. The attempt and current owner are
    /// checked in the same transaction so an abandoned worker cannot publish.
    async fn append_dvr_observation_event(
        &self,
        recording_id: &str,
        owner_node_id: &str,
        attempt: i64,
        event: &crate::dvr::DvrEventInput,
    ) -> Result<bool, StoreError>;

    /// Record that an owner recovered a capture without its prior worker.
    /// The owner/attempt predicate prevents a stale process from degrading a
    /// newer attempt's provenance.
    async fn mark_dvr_history_gap(
        &self,
        recording_id: &str,
        owner_node_id: &str,
        attempt: i64,
    ) -> Result<bool, StoreError>;

    async fn list_dvr_events(
        &self,
        recording_id: &str,
        before: Option<i64>,
        after: Option<i64>,
        limit: i64,
    ) -> Result<crate::dvr::DvrEventPage, StoreError>;

    /// Advance one user's review position. A zero sequence may create the
    /// supplied explicit legacy baseline for a pre-ledger terminal outcome.
    async fn acknowledge_dvr_attention(
        &self,
        recording_id: &str,
        user_id: i64,
        through_sequence: i64,
        acknowledged_at_ms: i64,
        legacy_baseline: &crate::dvr::DvrEventInput,
    ) -> Result<Option<i64>, StoreError>;

    async fn list_dvr_attention(
        &self,
        user_id: i64,
        section: crate::dvr::DvrAttentionSection,
        after: Option<(i64, &str)>,
        upper: Option<(i64, &str)>,
        limit: i64,
    ) -> Result<(Vec<crate::dvr::DvrAttentionRow>, i64), StoreError>;

    /// Delete at most `limit` oldest rows outside the age/server ceilings.
    async fn prune_dvr_events(&self, cutoff_ms: i64, limit: i64) -> Result<i64, StoreError>;

    async fn list_dvr_reminders(
        &self,
        user_id: i64,
        state: Option<crate::dvr::DvrReminderState>,
    ) -> Result<Vec<crate::dvr::DvrReminder>, StoreError>;

    /// Every reminder in this state across all users. The owner loop's read.
    async fn list_dvr_reminders_in(
        &self,
        state: crate::dvr::DvrReminderState,
    ) -> Result<Vec<crate::dvr::DvrReminder>, StoreError>;

    /// Upsert by id. Refuses past `DVR_REMINDERS_PER_USER_MAX`.
    async fn put_dvr_reminder(
        &self,
        reminder: &crate::dvr::DvrReminder,
    ) -> Result<bool, StoreError>;

    async fn delete_dvr_reminder(&self, user_id: i64, id: &str) -> Result<bool, StoreError>;

    /// Move `armed` reminders whose lead time has arrived to `fired`, and
    /// started ones to `expired`, returning the rows that fired.
    async fn transition_dvr_reminders(
        &self,
        now_seconds: i64,
        now_ms: i64,
    ) -> Result<Vec<crate::dvr::DvrReminder>, StoreError>;

    async fn set_dvr_reminder_state(
        &self,
        user_id: Option<i64>,
        id: &str,
        from: crate::dvr::DvrReminderState,
        to: crate::dvr::DvrReminderState,
        now_ms: i64,
    ) -> Result<bool, StoreError>;
}

#[async_trait]
pub trait MediaStore: Send + Sync + 'static {
    /// One bounded catalogue snapshot for deterministic duplicate-show repair.
    /// Implementations must return complete rows or an explicit size error;
    /// they must never silently truncate.
    async fn identity_repair_snapshot(
        &self,
        library_id: i64,
        show_ids: &[i64],
    ) -> Result<IdentityRepairSnapshot, StoreError>;
    /// The item carrying these external ids, across every library.
    ///
    /// For resolving something another application named. Matching on ids and
    /// never on titles is the rule this whole integration is built on — an
    /// application guessing which item you meant is the failure it exists to
    /// remove, and a title match here would reintroduce it from the other
    /// side.
    ///
    /// `kind` is required rather than inferred: a film and a show can hold the
    /// same TMDB id, because the two id spaces are separate.
    async fn item_by_external_id(
        &self,
        kind: ItemKind,
        tmdb_id: Option<i64>,
        imdb_id: Option<&str>,
    ) -> Result<Option<Item>, StoreError>;

    // --- item placement (scanner) ---
    async fn find_movie(
        &self,
        library_id: i64,
        title: &str,
        year: Option<i32>,
    ) -> Result<Option<Item>, StoreError>;
    async fn find_movies_by_directory(
        &self,
        library_id: i64,
        directory: &str,
    ) -> Result<Vec<Item>, StoreError>;
    async fn find_book(
        &self,
        library_id: i64,
        kind: ItemKind,
        title: &str,
        year: Option<i32>,
        identity_path: Option<&str>,
    ) -> Result<Option<Item>, StoreError>;
    async fn find_show(
        &self,
        library_id: i64,
        title: &str,
        year: Option<i32>,
    ) -> Result<Option<Item>, StoreError>;
    /// Directory candidates ordered with an owner of `season_number` first,
    /// then by `(added_at, id)`. Carrying the incoming season into this query
    /// avoids one catalogue round trip per duplicate show.
    async fn find_shows_by_directory(
        &self,
        library_id: i64,
        directory: &str,
        season_number: i32,
    ) -> Result<Vec<Item>, StoreError>;
    async fn find_season(
        &self,
        show_id: i64,
        season_number: i32,
    ) -> Result<Option<Item>, StoreError>;
    async fn find_episode(
        &self,
        season_id: i64,
        episode_number: i32,
    ) -> Result<Option<Item>, StoreError>;
    /// Find a child by (library, parent, kind, title) — how the home
    /// scanner's mirrored folder tree keeps its identity. `parent_id: None`
    /// matches items directly under a library root.
    async fn find_child_item(
        &self,
        library_id: i64,
        parent_id: Option<i64>,
        kind: ItemKind,
        title: &str,
    ) -> Result<Option<Item>, StoreError>;
    async fn insert_item(&self, item: &NewItem) -> Result<i64, StoreError>;

    // --- browse ---
    async fn get_item(&self, id: i64) -> Result<Option<Item>, StoreError>;
    /// Titles for a bounded set of item ids in one storage operation.
    /// Activity snapshots use this instead of issuing one authority read per
    /// viewer while building a response with a shorter transport deadline.
    async fn item_titles(&self, ids: &[i64]) -> Result<BTreeMap<i64, String>, StoreError>;
    async fn get_item_children(&self, parent_id: i64) -> Result<Vec<Item>, StoreError>;
    /// Every item that names at least one materialized artwork file.
    ///
    /// Cluster voters use this as a reconciliation inventory: item rows are
    /// replicated, while the bytes named by `poster_path` and `backdrop_path`
    /// remain node-local. This intentionally returns a narrow local projection
    /// so a periodic node-local pass does not rendezvous at the leader or move
    /// complete catalogue rows.
    async fn items_with_artwork(&self) -> Result<Vec<ArtworkInventoryItem>, StoreError>;
    /// One deterministic, bounded reconciliation page after an item id.
    async fn items_with_artwork_page(
        &self,
        after_item_id: i64,
        limit: i64,
    ) -> Result<Vec<Item>, StoreError>;
    /// Exact authority check immediately before orphan quarantine/removal.
    async fn artwork_filename_is_referenced(&self, filename: &str) -> Result<bool, StoreError>;
    /// Bounded batch authority snapshot for orphan prefiltering.
    async fn referenced_artwork_filenames(
        &self,
        filenames: &[String],
    ) -> Result<Vec<String>, StoreError>;
    /// One page of a library's grid, optionally narrowed to a single genre.
    ///
    /// The genre is matched against the item's stored list (migration v13),
    /// case-insensitively; `None` is the unfiltered query verbatim. It is a
    /// WHERE clause rather than a second code path on purpose — a filtered
    /// page that sorted or counted differently from an unfiltered one is the
    /// bug this shape cannot have.
    async fn list_top_items_in_genre(
        &self,
        library_id: i64,
        sort: ItemSort,
        offset: i64,
        limit: i64,
        genre: Option<&str>,
    ) -> Result<ItemPage, StoreError>;
    /// The unfiltered grid: every caller that had no opinion about genres
    /// before this existed and still has none. Provided rather than
    /// implemented so adding the parameter could not quietly change what any
    /// of them asks for.
    async fn list_top_items(
        &self,
        library_id: i64,
        sort: ItemSort,
        offset: i64,
        limit: i64,
    ) -> Result<ItemPage, StoreError> {
        self.list_top_items_in_genre(library_id, sort, offset, limit, None)
            .await
    }
    /// A recent-items preview for every library in one catalog query.
    ///
    /// This is the Store half of Home's constant-cardinality contract: the
    /// number of replicated authority reads must not grow with the library
    /// roster. Empty libraries are omitted and joined back in by the caller.
    async fn home_preview_pages(
        &self,
        limit_per_library: i64,
    ) -> Result<Vec<HomePreviewPage>, StoreError>;
    async fn recently_added(
        &self,
        library_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<RecentItem>, StoreError>;
    async fn search_items(&self, query: &str, limit: i64) -> Result<Vec<RecentItem>, StoreError>;

    // --- metadata enrichment ---
    async fn apply_series_tmdb_hint(
        &self,
        library_id: i64,
        show_id: i64,
        tmdb_id: i64,
    ) -> Result<SeriesHintOutcome, StoreError>;
    async fn apply_metadata(&self, item_id: i64, patch: &MetadataPatch) -> Result<(), StoreError>;
    /// Apply provider metadata only while the replicated
    /// owner/term/generation fence is still current. A stale submitted command
    /// succeeds as a no-op.
    async fn apply_metadata_if_artwork_repair_current(
        &self,
        item_id: i64,
        patch: &MetadataPatch,
        fence: &ArtworkRepairFence,
    ) -> Result<bool, StoreError>;
    /// Apply first-class facts for one book with source precedence enforced by
    /// the backend. A Curator handoff outranks EPUB package metadata; title +
    /// author alone never creates a work relation.
    async fn apply_book_metadata(
        &self,
        item_id: i64,
        patch: &BookMetadataPatch,
    ) -> Result<(), StoreError>;
    /// Apply Curator facts only while every book field this operation may
    /// overwrite is still the item snapshot the caller acted on. This is the
    /// store-level fence between slow provider work and a concurrent re-pair
    /// or edit on another voter.
    async fn apply_book_metadata_if_current(
        &self,
        expected: &Item,
        patch: &BookMetadataPatch,
        repair_fence: Option<&ArtworkRepairFence>,
    ) -> Result<bool, StoreError>;
    /// Book rows in one library, optionally narrowed to the exact ids a
    /// targeted scan placed. `Some(&[])` means no rows.
    async fn book_items(
        &self,
        library_id: i64,
        only: Option<&[i64]>,
    ) -> Result<Vec<Item>, StoreError>;
    /// Other text/audio editions carrying the same proven work id. The
    /// current row is excluded, and a missing work id is represented by the
    /// caller not asking at all—not by fuzzy title/author matching.
    async fn related_book_editions(
        &self,
        item_id: i64,
        work_id: &str,
    ) -> Result<Vec<Item>, StoreError>;
    /// Movies and shows to enrich: normally those no provider has answered
    /// for yet, which is *not* the same as "those with no TMDB id" — an item
    /// can arrive carrying an id from another application (a Curator scan
    /// request) and still need every other field. `force` includes
    /// already-enriched items too (a metadata refresh, e.g. to backfill
    /// season posters onto shows enriched before that existed).
    ///
    /// `only` narrows the result to specific item ids — what a targeted scan
    /// enriches, so that a request another application is *waiting on* costs
    /// the handful of items it just delivered rather than a whole library.
    /// `None` means "no id filter" and is the unnarrowed query verbatim;
    /// `Some(&[])` means "these zero items", i.e. nothing.
    async fn items_needing_metadata(
        &self,
        library_id: Option<i64>,
        force: bool,
        only: Option<&[i64]>,
    ) -> Result<Vec<Item>, StoreError>;
    /// All episodes of a show (across seasons), for bulk episode enrichment.
    async fn episodes_for_show(&self, show_id: i64) -> Result<Vec<Item>, StoreError>;
    /// Home-library items whose artwork the local enricher should generate:
    /// folders, videos, and photos with no poster yet (`force` = all of them).
    /// Folders come last so they can inherit a child's finished poster.
    /// `only` narrows to specific ids, as on
    /// [`items_needing_metadata`](Self::items_needing_metadata).
    async fn items_needing_artwork(
        &self,
        library_id: i64,
        force: bool,
        only: Option<&[i64]>,
    ) -> Result<Vec<Item>, StoreError>;
    /// Provider-enriched items still carrying incomplete artwork, oldest
    /// attempt first — the retry sweep's input, and the mirror of
    /// [`files_missing_probe`](Self::files_missing_probe).
    ///
    /// `retry_after_secs` is the backoff: an item attempted more recently than
    /// that is skipped. Without it an item TMDB genuinely has no art for would
    /// be re-fetched on every single cycle, forever, which is how a
    /// self-healing job turns into a rate-limit generator.
    /// `limit` bounds one pass so a large upgrade backlog cannot monopolize
    /// the scheduler or provider connection for hours. A non-positive limit
    /// requests no work; callers must opt into an explicit positive bound.
    async fn items_missing_artwork(
        &self,
        library_id: Option<i64>,
        retry_after_secs: i64,
        limit: i64,
    ) -> Result<Vec<Item>, StoreError>;
    /// Movies and shows with no genres yet, in ascending id order, starting
    /// strictly after `after_id` — the genre backfill's input.
    ///
    /// It cannot reuse [`items_needing_metadata`](Self::items_needing_metadata):
    /// every item this is for is already `metadata_at`-stamped and therefore
    /// invisible to that query, which is the whole reason a backfill exists.
    ///
    /// Ordered by id and cut at `after_id` because that pair IS the
    /// resumability: the caller stamps the last id it finished, and a crash,
    /// a restart or a 429 resumes from there instead of paying for the
    /// catalogue twice. A `LIMIT` rather than the whole list so one pass is
    /// bounded work against a rate-limited API.
    async fn items_missing_genres(
        &self,
        after_id: i64,
        limit: i64,
    ) -> Result<Vec<Item>, StoreError>;
    /// Apply a hand edit — distinct from [`apply_metadata`](Self::apply_metadata)
    /// because an edit must be able to *clear* a field. Returns the updated
    /// item, or `None` if the id doesn't exist.
    async fn update_item_fields(
        &self,
        item_id: i64,
        edit: &ItemEdit,
    ) -> Result<Option<Item>, StoreError>;
    /// Record that an NFO sidecar has been consumed for this item. Seeding
    /// happens at most once, ever (docs/features/HOMEVIDEO-PLAN.md §4.3) — after this
    /// the sidecar is dead to plurx, so a user's edits can never be clobbered.
    async fn set_nfo_seeded(&self, item_id: i64) -> Result<(), StoreError>;

    // --- files ---
    async fn get_file_by_path(&self, path: &str) -> Result<Option<MediaFile>, StoreError>;
    async fn upsert_file(
        &self,
        item_id: i64,
        path: &str,
        size: i64,
        mtime: i64,
        probe: &ProbeResult,
    ) -> Result<i64, StoreError>;
    async fn get_file(&self, id: i64) -> Result<Option<MediaFile>, StoreError>;
    /// False means duplicate, full, or a source revision replaced during download.
    async fn add_downloaded_subtitle(
        &self,
        file_id: i64,
        track: &crate::domain::DownloadedSubtitle,
    ) -> Result<bool, StoreError>;
    /// Bounded, ordered catalog walk for optional subtitle acquisition.
    async fn subtitle_candidate_file_ids(
        &self,
        after_id: i64,
        limit: i64,
    ) -> Result<Vec<i64>, StoreError>;
    /// A census of what the libraries actually hold, in transcoder terms.
    ///
    /// Aggregated in SQL rather than by walking files: a library of a few
    /// hundred thousand rows should cost one scan, not one round trip per
    /// title, or nobody will run it twice.
    async fn media_shape(&self) -> Result<MediaShape, StoreError>;
    async fn files_for_item(&self, item_id: i64) -> Result<Vec<MediaFile>, StoreError>;
    /// How many children each of the given items has. Folder cards say "12
    /// items"; doing that with one query per card would be an N+1 on every
    /// grid render.
    async fn child_counts(
        &self,
        ids: &[i64],
    ) -> Result<std::collections::HashMap<i64, i64>, StoreError>;
    /// Best (max) probed file height per item, for the given item ids. Used to
    /// badge/section the library grid by resolution without loading every file.
    async fn item_max_heights(
        &self,
        ids: &[i64],
    ) -> Result<std::collections::HashMap<i64, i64>, StoreError>;
    /// Aggregated media facts per item, for the given item ids — the `media`
    /// block on library and season listings.
    ///
    /// One statement for the whole page, exactly like `item_max_heights`: this
    /// feeds a list, so a per-item lookup is a 200-round-trip page render, and
    /// the obvious alternative (load every file of every item and reduce in
    /// Rust) is that same fan-out wearing a different hat. Items with no files
    /// are absent from the map rather than present-and-empty — the caller
    /// decorates what it got and leaves the rest bare.
    async fn item_media_facts(
        &self,
        ids: &[i64],
    ) -> Result<std::collections::HashMap<i64, MediaFacts>, StoreError>;
    /// Persist a manual A/V sync correction for one file (0 clears it).
    async fn set_file_audio_offset(&self, file_id: i64, offset_ms: i64) -> Result<(), StoreError>;
    /// Dolby Vision files whose configuration columns are still empty, with
    /// the probe JSON to fill them from and the display label they currently
    /// carry — strictly after `after_id`, lowest first, bounded.
    ///
    /// The M2 backfill's input, and the cursor is what makes it terminate.
    /// Some of these rows can never be fixed — a Dolby Vision codec tag whose
    /// stored probe has no configuration record in it needs a real re-probe,
    /// not a re-read — and without a cursor they stay in the answer forever,
    /// re-read every tick, hiding every fixable row behind them.
    async fn files_missing_dolby_vision(
        &self,
        after_id: i64,
        limit: i64,
    ) -> Result<Vec<(i64, String, Option<String>)>, StoreError>;
    /// Rows whose stored probe can supply the first playable video's sample
    /// entry, strictly after `after_id` and in ascending id order.
    async fn files_missing_video_codec_tag(
        &self,
        after_id: i64,
        limit: i64,
    ) -> Result<Vec<MissingVideoCodecTag>, StoreError>;
    /// Write a recovered tag only if the row is still the exact source and
    /// probe snapshot returned by `files_missing_video_codec_tag`.
    async fn set_file_video_codec_tag(
        &self,
        candidate: &MissingVideoCodecTag,
        video_codec_tag: &str,
    ) -> Result<bool, StoreError>;
    /// Rows whose retained probe document can populate the additive field
    /// order column, strictly after `after_id` and bounded.
    async fn files_missing_field_order(
        &self,
        after_id: i64,
        limit: i64,
    ) -> Result<Vec<MissingFieldOrder>, StoreError>;
    /// Write the recovered token only while every source and probe identity
    /// field still matches the snapshot returned above.
    async fn set_file_field_order(
        &self,
        candidate: &MissingFieldOrder,
        field_order: &str,
    ) -> Result<bool, StoreError>;
    /// HDR rows whose additive luminance columns have not been classified.
    /// The same identity projection as the codec-tag backfill keeps the
    /// subsequent write fenced to this exact stored probe snapshot.
    async fn files_missing_luminance(
        &self,
        after_id: i64,
        limit: i64,
    ) -> Result<Vec<MissingVideoCodecTag>, StoreError>;
    async fn set_file_luminance(
        &self,
        candidate: &MissingVideoCodecTag,
        max_cll: Option<i64>,
        max_fall: Option<i64>,
        mastering_max_luminance: Option<i64>,
        source: &str,
    ) -> Result<bool, StoreError>;
    /// Write one file's Dolby Vision columns, and the display label derived
    /// from them.
    ///
    /// The label moves with the columns because they are the same fact: a row
    /// whose scan produced the bare string "Dolby Vision" while its probe JSON
    /// carried a profile has a label that is wrong, not merely sparse, and
    /// leaving it would keep that file unclaimable by every client.
    async fn set_file_dolby_vision(
        &self,
        file_id: i64,
        facts: DolbyVisionFacts,
        hdr_format: Option<&str>,
    ) -> Result<(), StoreError>;
    /// The raw ffprobe JSON captured at scan time (for the declared per-stream
    /// start-time readout in the player's sync menu, and the chapter markers
    /// the player shows as Skip Intro / Skip Credits).
    async fn get_file_probe_json(&self, file_id: i64) -> Result<Option<String>, StoreError>;
    /// The stored `chapters` array projected in SQL, without materializing the
    /// unrelated (and potentially much larger) ffprobe document in the caller.
    async fn get_file_probe_chapters_json(
        &self,
        file_id: i64,
    ) -> Result<Option<String>, StoreError>;
    /// Graft a `chapters` array onto a file's stored probe JSON.
    ///
    /// Only for files probed before chapters were captured at scan time: the
    /// play path probes such a file once, then writes the answer here so the
    /// next play reads it like any other. `chapters_json` is a JSON array.
    /// A file with no stored probe is left alone — there is nothing to graft
    /// onto, and inventing a document would fake a successful probe.
    async fn merge_file_probe_chapters(
        &self,
        file_id: i64,
        chapters_json: &str,
    ) -> Result<(), StoreError>;
    /// Files whose probe never succeeded (`probe_json IS NULL`), oldest scan
    /// first. `library_id` narrows to one library; `None` is server-wide. These
    /// are the records the retry job and the scan's repair pass exist for —
    /// nothing about them changes on disk when the reason they failed is fixed.
    async fn files_missing_probe(
        &self,
        library_id: Option<i64>,
    ) -> Result<Vec<MediaFile>, StoreError>;
    /// All known file paths in a library (for vanished-file detection).
    async fn library_file_paths(&self, library_id: i64) -> Result<Vec<(i64, PathBuf)>, StoreError>;
    /// Establish the stable identity of the mounted roots for this library,
    /// or compare a later scanner's observation with cluster truth.
    async fn ensure_library_root_fingerprint(
        &self,
        library_id: i64,
        fingerprint: &str,
        allow_establish: bool,
    ) -> Result<RootFingerprintStatus, StoreError>;
    /// Forget a library's recorded root identity after an operator has
    /// verified a deliberate mount/path replacement.
    async fn reset_library_root_fingerprint(&self, library_id: i64) -> Result<bool, StoreError>;
    /// Recreate the derived full-text index from authoritative item rows.
    async fn rebuild_search_index(&self) -> Result<u64, StoreError>;
    /// Atomically remove vanished files and their empty item hierarchy only
    /// when the scanner sees the expected roots and stays within its deletion
    /// budget. This is the M1c publication boundary; M4 adds its lease fence.
    async fn reconcile_library(
        &self,
        library_id: i64,
        root_fingerprint: &str,
        gone_file_ids: &[i64],
        prune_limit: u64,
    ) -> Result<ReconcileOutcome, StoreError>;
    async fn delete_files(&self, ids: &[i64]) -> Result<u64, StoreError>;
    /// Remove items left childless/file-less after a scan. Returns rows removed.
    async fn prune_empty_items(&self, library_id: i64) -> Result<u64, StoreError>;
}

/// Exactly the items displayed by a top-level library grid.
///
/// Shared by ordinary library pages and the catalog-wide Home preview so the
/// two surfaces cannot silently acquire different membership rules.
pub(crate) const TOP_LEVEL_ITEM_PREDICATE: &str =
    "(kind IN ('movie','show','book','audiobook') OR \
     (kind IN ('folder','video','photo') AND parent_id IS NULL))";

/// The `ORDER BY` for a library grid page, for every backend and every sort.
///
/// **Every clause ends in `id`, and that is the point.** `sort_title` is not
/// unique — "Harbor Lights" the 1947 film and "Harbor Lights" the 1971 film
/// reduce to the same key — and neither are `year`, a resolution or a capture
/// date. Without a unique final key SQLite is free to return equal rows in
/// any order it likes, and it does not have to pick the same one twice. A
/// client paging by `offset` then asks for rows 0..199 and rows 200..399 of
/// two different orderings, so an item on the seam is shown twice and its
/// neighbour is never shown at all. That is invisible on a small library and
/// certain on a large one, and it is why a client cannot be asked to merge
/// several of these pages into one grid until the order is total.
///
/// `Added` already ended in `id DESC` and keeps it: giving it `id ASC` for
/// symmetry would reorder equal-`added_at` rows that viewers see today, for
/// nothing. The four that gain `id ASC` had no tie-break at all.
///
/// One function rather than one per backend because the SQLite and Hiqlite
/// media stores each spelled this out, three copies in total, and a merge
/// order that differs between backends is a bug no single-backend test can
/// see. Native clients merge library cursors against this exact order, so it
/// is also pinned from the outside by `tests/contracts/library-sort-cases.json`.
pub(crate) fn item_sort_order_by(sort: ItemSort) -> &'static str {
    match sort {
        ItemSort::Title => "sort_title ASC, id ASC",
        ItemSort::Added => "added_at DESC, id DESC",
        ItemSort::Year => "year IS NULL, year DESC, sort_title ASC, id ASC",
        // Best (max) file height per item, highest first; no-height items
        // last. Only the kinds that carry a `resolution` on the DTO
        // (`ItemKind::carries_resolution`) are ranked by height: a root photo
        // has a real file height but no `resolution`, and ranking it by one
        // would hand a merging client a cursor that is not sorted under the
        // key it was given.
        ItemSort::Resolution => {
            "CASE WHEN kind IN ('movie','video') \
             THEN COALESCE((SELECT MAX(f.height) FROM files f WHERE f.item_id = items.id), -1) \
             ELSE -1 END DESC, \
             sort_title ASC, id ASC"
        }
        ItemSort::Recorded => "(recorded_at IS NULL), recorded_at DESC, sort_title ASC, id ASC",
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RootFingerprintStatus {
    Established,
    Matched,
    Unestablished,
    Mismatch { expected: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReconcileOutcome {
    Applied {
        deleted_files: u64,
        pruned_items: u64,
    },
    RefusedRoot {
        expected: String,
    },
    RefusedPrune {
        requested: u64,
        limit: u64,
    },
}

/// Hard ceiling for one atomic library-reconcile payload. At 4,096 signed
/// decimal i64 values, the JSON carried by the replicated backend stays below
/// 90 KiB even at worst-case width.
pub(crate) const MAX_RECONCILE_FILE_IDS: usize = 4_096;

pub(crate) fn reconcile_payload_refusal(
    gone_file_ids: &[i64],
    prune_limit: u64,
) -> Option<ReconcileOutcome> {
    (gone_file_ids.len() > MAX_RECONCILE_FILE_IDS).then(|| ReconcileOutcome::RefusedPrune {
        requested: gone_file_ids.len() as u64,
        limit: prune_limit.min(MAX_RECONCILE_FILE_IDS as u64),
    })
}

#[async_trait]
pub trait WatchStore: Send + Sync + 'static {
    async fn watch_state(
        &self,
        user_id: i64,
        item_id: i64,
    ) -> Result<Option<WatchState>, StoreError>;
    /// Batch lookup for annotating item lists.
    async fn watch_map(
        &self,
        user_id: i64,
        item_ids: &[i64],
    ) -> Result<Vec<(i64, WatchState)>, StoreError>;
    /// Record playback progress; crossing 95% marks watched automatically.
    async fn put_progress(
        &self,
        user_id: i64,
        item_id: i64,
        position_ms: i64,
        duration_ms: Option<i64>,
    ) -> Result<WatchState, StoreError> {
        self.put_progress_at(user_id, item_id, position_ms, duration_ms, None)
            .await
    }
    /// Commit a coalesced playback beat only if the durable row still matches
    /// the state observed by the coalescer's leading write.
    ///
    /// A manual watched/unwatched action, an offline replay, or a Trakt merge
    /// may update the same row while a trailing beat is waiting. Returning
    /// `None` tells the coalescer that another writer won; it must discard the
    /// stale beat rather than overwrite that newer intent.
    async fn put_progress_if_current(
        &self,
        user_id: i64,
        item_id: i64,
        expected: &WatchState,
        position_ms: i64,
        duration_ms: Option<i64>,
    ) -> Result<Option<WatchState>, StoreError>;
    /// Record playback progress with an optional client observation time.
    ///
    /// `recorded_at` is used by offline clients replaying their durable final
    /// state after reconnecting. A write older than the stored watch state is
    /// ignored, which prevents a late phone sync from rewinding playback that
    /// already continued on another device. Omitting it uses server time and
    /// preserves the ordinary online-heartbeat behavior.
    async fn put_progress_at(
        &self,
        user_id: i64,
        item_id: i64,
        position_ms: i64,
        duration_ms: Option<i64>,
        recorded_at: Option<i64>,
    ) -> Result<WatchState, StoreError>;
    /// Flip one item's flag and nothing else. Callers acting on something a
    /// person clicked want [`WatchStore::set_watched_tree`] instead — this is
    /// the single-row primitive it is built from.
    async fn set_watched(
        &self,
        user_id: i64,
        item_id: i64,
        watched: bool,
    ) -> Result<(), StoreError>;
    /// Mark everything playable under `item_id` — and the item itself when it
    /// is playable — watched or unwatched. Returns the ids that actually
    /// changed, so a caller can notify on those and stay quiet about the rest.
    ///
    /// Containers are the point. A show is not something you watch; its
    /// episodes are, and "I've seen this series" is a statement about all of
    /// them. Marking only the show row would leave every episode unwatched
    /// underneath it, which is worse than not offering the button: the badge
    /// would say watched while Next Up went on offering episode one.
    async fn set_watched_tree(
        &self,
        user_id: i64,
        item_id: i64,
        watched: bool,
    ) -> Result<Vec<i64>, StoreError>;
    /// Count the playable leaves under `item_id` and how many of them are
    /// watched. A playable item is its own leaf, so this answers for movies
    /// too — a movie is 1/1 or 0/1.
    async fn watch_rollup(&self, user_id: i64, item_id: i64) -> Result<WatchRollup, StoreError>;
    /// [`WatchStore::watch_rollup`] for a whole page of containers, in one
    /// query.
    ///
    /// A library grid needs this for every show and season it paints, and a
    /// rollup is a recursive walk: doing it per card is an N+1 that grows
    /// with the season count, not the page size. Same contract as the single
    /// version, including the answer for a container holding nothing playable
    /// — every requested id gets an entry, `0/0` if its subtree has no
    /// leaves, so a caller never has to guess whether a missing key means
    /// "empty" or "not asked".
    async fn watch_rollups(
        &self,
        user_id: i64,
        ids: &[i64],
    ) -> Result<std::collections::HashMap<i64, WatchRollup>, StoreError>;
    async fn continue_watching(
        &self,
        user_id: i64,
        limit: i64,
    ) -> Result<Vec<InProgressItem>, StoreError>;
    /// Next-up episodes: for each show the user has watched into, the first
    /// unwatched, not-in-progress episode after the last watched one. Pairs
    /// with continue-watching (resume) — this is "start the next episode".
    async fn next_up(&self, user_id: i64, limit: i64) -> Result<Vec<RecentItem>, StoreError>;
    /// [`WatchStore::watch_map`] over `item_ids` and
    /// [`WatchStore::watch_rollups`] over `container_ids`, answered from one
    /// read of the same state: one replicated statement, or one SQLite read
    /// transaction. Same per-half contracts as the two methods it replaces.
    async fn watch_summary(
        &self,
        user_id: i64,
        item_ids: &[i64],
        container_ids: &[i64],
    ) -> Result<WatchSummary, StoreError>;
    /// [`WatchStore::continue_watching`] and [`WatchStore::next_up`] from one
    /// read of the same progress state, each rail in its own established
    /// order and limit.
    async fn progress_rails(&self, user_id: i64, limit: i64) -> Result<ProgressRails, StoreError>;
    /// Write a watch fact that arrived from an external source (Trakt sync):
    /// unlike [`put_progress`] the caller controls `updated_at`, so remote
    /// timestamps land verbatim and later merges compare correctly.
    async fn apply_remote_watch(
        &self,
        user_id: i64,
        item_id: i64,
        watched: bool,
        position_ms: i64,
        duration_ms: Option<i64>,
        updated_at: i64,
    ) -> Result<(), StoreError>;
}

/// Per-user text-publication state. It is separate from [`WatchStore`]
/// because a reflowable locator is not timed playback and completion is an
/// explicit reader action rather than a 95% threshold.
#[async_trait]
pub trait ReadingStore: Send + Sync + 'static {
    /// Read the row for one edition, including a stale revision retained for
    /// diagnosis. The HTTP boundary compares its revision to the current
    /// file before offering a resume action.
    async fn reading_state(
        &self,
        user_id: i64,
        item_id: i64,
        file_id: i64,
    ) -> Result<Option<ReadingState>, StoreError>;
    /// Newest state whose snapshotted revision still matches the current file.
    /// Item detail uses this one query instead of looking up every edition.
    async fn current_reading_state(
        &self,
        user_id: i64,
        item_id: i64,
    ) -> Result<Option<ReadingState>, StoreError>;
    /// Persist a locator. A dated offline write older than the durable row
    /// returns the winner without rewinding it; an undated online write uses
    /// the server clock and is authoritative now. A different file revision
    /// begins a fresh ordering epoch because the previous locator is stale.
    async fn put_reading_state(
        &self,
        user_id: i64,
        item_id: i64,
        state: &ReadingStateWrite,
    ) -> Result<ReadingState, StoreError>;
    /// Clear one edition's progress. Idempotent so Start over can be retried.
    async fn delete_reading_state(
        &self,
        user_id: i64,
        item_id: i64,
        file_id: i64,
    ) -> Result<(), StoreError>;
}

/// Trakt account links and the identity join sync needs.
#[async_trait]
pub trait TraktStore: Send + Sync + 'static {
    async fn get_trakt_auth(&self, user_id: i64) -> Result<Option<TraktAuth>, StoreError>;
    async fn list_trakt_auth(&self) -> Result<Vec<TraktAuth>, StoreError>;
    async fn put_trakt_auth(&self, auth: &TraktAuth) -> Result<(), StoreError>;
    async fn delete_trakt_auth(&self, user_id: i64) -> Result<(), StoreError>;
    /// Delete a rejected OAuth link only if no other voter has already
    /// rotated the refresh token.
    ///
    /// The compare-and-set operand is the stored envelope, not the cleartext
    /// token: every voter sees the same ciphertext for a given row, so this
    /// stays an exact equality check without any node unwrapping anything.
    async fn delete_trakt_auth_if_current(
        &self,
        user_id: i64,
        expected_refresh_token: &SealedSecret,
    ) -> Result<bool, StoreError>;
    /// Refresh bookkeeping after a token rotation. The caller seals the new
    /// tokens; the store never sees a bearer credential in the clear.
    async fn update_trakt_tokens(
        &self,
        user_id: i64,
        expected_refresh_token: &SealedSecret,
        access_token: &SealedSecret,
        refresh_token: &SealedSecret,
        expires_at: i64,
    ) -> Result<bool, StoreError>;
    /// Stamp a completed sync run (and the last_activities gate JSON).
    async fn set_trakt_sync(
        &self,
        user_id: i64,
        last_sync_at: i64,
        last_activities: Option<&str>,
    ) -> Result<(), StoreError>;
    /// Every movie/episode with a Trakt-matchable identity, plus the user's
    /// watch row and a fallback duration — the sync planner's input.
    async fn trakt_sync_candidates(
        &self,
        user_id: i64,
    ) -> Result<Vec<crate::trakt::SyncCandidate>, StoreError>;
}

/// Scoped API keys — the machine credential, kept deliberately separate
/// from [`UserStore`] because a key is not a user and must never be able to
/// become one.
#[async_trait]
pub trait ApiKeyStore: Send + Sync + 'static {
    /// Store a key. Only the SHA-256 hash of the secret is persisted; the
    /// caller shows the plaintext once and then forgets it.
    async fn create_api_key(
        &self,
        name: &str,
        key_hash: &str,
        scopes: &[String],
    ) -> Result<crate::domain::ApiKey, StoreError>;
    async fn list_api_keys(&self) -> Result<Vec<crate::domain::ApiKey>, StoreError>;
    async fn api_key_for_hash(
        &self,
        key_hash: &str,
    ) -> Result<Option<crate::domain::ApiKey>, StoreError>;
    /// Record that a key was just used — the only way an operator can tell a
    /// forgotten key from a working one.
    async fn touch_api_key(&self, id: i64) -> Result<(), StoreError>;
    async fn delete_api_key(&self, id: i64) -> Result<bool, StoreError>;
    async fn set_api_key_disabled(&self, id: i64, disabled: bool) -> Result<bool, StoreError>;
}

/// One queued outbound notification.
#[derive(Clone, Debug)]
pub struct OutboxEntry {
    pub id: i64,
    pub payload: String,
    pub attempts: i64,
    pub last_error: String,
    /// pending | ok | failed
    pub status: String,
    pub next_at: i64,
    /// Lease deadline for the worker that fetched this row. A stale worker's
    /// settlement is ignored after another voter reclaims the delivery.
    pub claim_until: i64,
}

/// The outbox for watched notifications (master plan §11.1).
///
/// A table rather than a channel, because this is plurx's first *outbound*
/// push. Answering a request needs no durability — the caller is still there
/// to be told. Pushing is the opposite: the moment that matters is one where
/// the far side may be restarting, and nobody is waiting to retry for us.
#[async_trait]
pub trait WatchedOutboxStore: Send + Sync + 'static {
    async fn enqueue_watched(&self, payload: &str) -> Result<i64, StoreError>;
    async fn due_watched(&self, limit: i64) -> Result<Vec<OutboxEntry>, StoreError>;
    async fn settle_watched(&self, entry: &OutboxEntry) -> Result<(), StoreError>;
    /// `(pending, ok, failed)` — for the settings page and `/metrics`.
    async fn watched_outbox_counts(&self) -> Result<(i64, i64, i64), StoreError>;
}

/// The pre-transcode cache: what has been produced, and where a copy is.
///
/// Split into recipes and locations at the schema level because a cluster
/// needs to say "node A has this, node B had it and evicted it" (PERF-PLAN
/// §6.1). Nothing here exposes that split, because no caller has ever wanted
/// it: the question at every call site is "is there a usable copy" or "make a
/// note that there is one", and a two-table join is an implementation detail
/// of answering it.
#[async_trait]
pub trait TranscodeCacheStore: Send + Sync + 'static {
    /// A complete local copy of `recipe_hash`, if one exists. `None` covers
    /// both "never produced" and "produced but still being written", which are
    /// the same answer to the only question a player asks.
    async fn cache_hit(
        &self,
        recipe_hash: &str,
        node_id: &str,
    ) -> Result<Option<CachedTranscode>, StoreError>;

    /// Claim a directory for a recipe about to be produced.
    ///
    /// Returns whether THIS caller took the claim. `false` means somebody got
    /// there first and is either producing it now or has already finished, and
    /// the answer matters: without it a second producer cannot tell "I own
    /// this" from "somebody else does", so it either duplicates hours of encode
    /// or — worse — publishes over a directory another process is writing.
    /// Idempotent in the sense that matters: repeated claims never move the
    /// first one's directory.
    async fn claim_cache_entry(
        &self,
        recipe_hash: &str,
        file_id: i64,
        recipe_version: i64,
        node_id: &str,
        relative_dir: &str,
    ) -> Result<bool, StoreError>;

    /// Say that a claim is still being worked on.
    ///
    /// Distinct from [`TranscodeCacheStore::touch_cache_entry`], which records
    /// that somebody *watched* an entry and drives eviction. This records that
    /// a producer is still making one, and drives the opposite decision — the
    /// crash sweep. Sharing a timestamp would mean a producer's progress made
    /// its output look freshly watched, and eviction would start protecting
    /// something nobody has ever played.
    async fn touch_cache_claim(&self, recipe_hash: &str, node_id: &str) -> Result<(), StoreError>;

    /// Mark a claim finished and serveable, with its measured size. Until this
    /// runs the entry is invisible to [`TranscodeCacheStore::cache_hit`].
    ///
    /// `manifest_digest` is `None` for a generation published without one,
    /// which is every generation under the unqualified artifact identity.
    ///
    /// The two states are not interchangeable, so the parameter is not
    /// defaulted: a row that records a digest is offered to cluster placement,
    /// is enrolled in the integrity scrub, and serves offline segments fatally
    /// rather than leniently on pre-existing bit rot. It is not the reuse
    /// gate — whether a generation may be reused as a health-qualified
    /// artifact is read from the manifest file beside its bytes, never from
    /// this row, because a row can outlive the bytes it describes.
    ///
    /// An implementation must treat `None` as "this publication carries no
    /// digest", never as "clear the digest this row already has".
    async fn complete_cache_entry(
        &self,
        recipe_hash: &str,
        node_id: &str,
        bytes: i64,
        manifest_digest: Option<&str>,
    ) -> Result<(), StoreError>;

    /// Note that somebody watched it — the LRU clock.
    async fn touch_cache_entry(&self, recipe_hash: &str, node_id: &str) -> Result<(), StoreError>;

    /// Complete local entries, coldest first. What eviction walks.
    async fn cache_by_age(
        &self,
        node_id: &str,
        limit: i64,
    ) -> Result<Vec<CachedTranscode>, StoreError>;

    /// Bounded set of manifest-fenced local generations for background
    /// integrity scrubbing. Unlike the eviction view this includes pinned
    /// offline locations: pinning protects valid bytes from LRU, not corrupt
    /// bytes from invalidation.
    async fn cache_manifest_candidates(
        &self,
        node_id: &str,
        limit: i64,
    ) -> Result<Vec<CachedTranscode>, StoreError>;

    /// Advance the scrub cursor only if the checked immutable publication is
    /// still current. Reusing `last_seen_at` rotates a bounded oldest-first
    /// scan without changing the playback LRU clock.
    async fn mark_cache_manifests_checked(
        &self,
        checks: &[CacheManifestCheck],
    ) -> Result<usize, StoreError>;

    /// Claims older than `older_than_unix` that never completed — a producer
    /// that died. Their directories are garbage and their rows are lies.
    async fn stale_cache_claims(
        &self,
        node_id: &str,
        older_than_unix: i64,
    ) -> Result<Vec<CachedTranscode>, StoreError>;

    /// Every local cache row, complete or claimed, without eviction policy.
    /// The filesystem orphan pass needs storage ownership facts; filtered LRU
    /// and stale-candidate queries are deliberately the wrong truth for it.
    async fn all_cache_rows(&self, node_id: &str) -> Result<Vec<CachedTranscode>, StoreError>;

    /// At most 10,000 local ownership rows plus an explicit completeness bit.
    /// Orphan cleanup may delete only when this view is complete, including a
    /// successful complete view containing zero rows.
    async fn cache_ownership_inventory(
        &self,
        node_id: &str,
    ) -> Result<crate::domain::CacheOwnershipInventory, StoreError>;

    /// Exact ownership for one bounded orphan-deletion batch. This remains
    /// complete even when a node has more rows than the broad inventory's
    /// housekeeping ceiling, so a large healthy cache cannot permanently
    /// disable reclamation.
    async fn cache_candidate_owners(
        &self,
        node_id: &str,
        relative_dirs: &[String],
        incomplete_recipes: &[String],
    ) -> Result<Vec<CachedTranscode>, StoreError>;

    /// Remove only the location whose immutable publication identity still
    /// matches a failed integrity check and atomically fail ready offline
    /// packages bound to it. A stale reader can neither erase a newer
    /// replacement nor retire packages backed by that replacement.
    async fn invalidate_cache_entry(
        &self,
        recipe_hash: &str,
        node_id: &str,
        storage_class: &str,
        relative_dir: &str,
        manifest_digest: Option<&str>,
    ) -> Result<bool, StoreError>;

    /// Forget one storage copy. The recipe row goes too when its last copy
    /// does: a recipe nobody has is not a fact worth keeping on a single node,
    /// and on a cluster the other nodes' rows keep it alive.
    async fn forget_cache_entry(
        &self,
        recipe_hash: &str,
        node_id: &str,
        storage_class: &str,
    ) -> Result<(), StoreError>;

    /// Complete local bytes charged to the ordinary playback-cache budget.
    /// Offline-pinned recipes have their own admission budget and are excluded
    /// so a flight queue cannot evict the playback cache to make room.
    async fn cache_bytes(&self, node_id: &str) -> Result<i64, StoreError>;
}

/// Storage-keyed cache generations and their distributed reader pins.
///
/// The legacy TranscodeCacheStore remains the rolling-upgrade interface for
/// node-local roots. Shared roots use this boundary exclusively: callers must
/// name the immutable generation they validated, and GC must present the exact
/// live lease that authorized retirement.
#[async_trait]
pub trait SharedCacheStore: Send + Sync + 'static {
    async fn put_cache_storage_member(&self, member: &CacheStorageMember)
        -> Result<(), StoreError>;

    async fn cache_storage_member(
        &self,
        storage_id: &str,
        node_id: &str,
    ) -> Result<Option<CacheStorageMember>, StoreError>;

    async fn mark_cache_storage_suspect(
        &self,
        storage_id: &str,
        node_id: &str,
        observed_at_ms: i64,
    ) -> Result<bool, StoreError>;

    async fn shared_cache_hit(
        &self,
        recipe_hash: &str,
        storage_id: &str,
    ) -> Result<Option<SharedCacheGeneration>, StoreError>;

    async fn touch_shared_cache_entry(
        &self,
        recipe_hash: &str,
        storage_id: &str,
        generation_id: &str,
        now_ms: i64,
    ) -> Result<bool, StoreError>;

    #[allow(clippy::too_many_arguments)]
    async fn claim_shared_cache_entry(
        &self,
        recipe_hash: &str,
        file_id: i64,
        recipe_version: i64,
        storage_id: &str,
        generation_id: &str,
        relative_dir: &str,
        now_ms: i64,
    ) -> Result<bool, StoreError>;

    #[allow(clippy::too_many_arguments)]
    async fn complete_shared_cache_entry(
        &self,
        recipe_hash: &str,
        storage_id: &str,
        generation_id: &str,
        bytes: i64,
        manifest_digest: &str,
        now_ms: i64,
    ) -> Result<bool, StoreError>;

    /// Fence only the exact still-incomplete publication claim into durable
    /// cleanup state. A stale or commit-uncertain publisher must never delete
    /// a completed replacement.
    async fn abandon_shared_cache_entry(
        &self,
        recipe_hash: &str,
        storage_id: &str,
        generation_id: &str,
        relative_dir: &str,
    ) -> Result<bool, StoreError>;

    /// Forget an exact abandoned claim only after its derived staging/final
    /// paths have been removed. A crash before this call remains discoverable.
    async fn finalize_abandoned_shared_cache_entry(
        &self,
        recipe_hash: &str,
        storage_id: &str,
        generation_id: &str,
        relative_dir: &str,
    ) -> Result<bool, StoreError>;

    /// Return a bounded oldest-first inventory of incomplete publication
    /// claims whose owner has exceeded the publication recovery window.
    async fn stale_shared_cache_claims(
        &self,
        storage_id: &str,
        before_ms: i64,
        limit: i64,
    ) -> Result<Vec<SharedCacheGeneration>, StoreError>;

    /// Insert or renew a pin only while the exact complete generation remains
    /// current. Validation and mutation are one database statement/transaction.
    async fn acquire_cache_consumer_pin(
        &self,
        pin: &CacheConsumerPin,
        now_ms: i64,
    ) -> Result<bool, StoreError>;

    /// Renew matching epochs in one bounded owner-liveness transaction.
    async fn renew_cache_consumer_pins(
        &self,
        pins: &[CacheConsumerPin],
        now_ms: i64,
    ) -> Result<usize, StoreError>;

    async fn release_cache_consumer_pin(
        &self,
        storage_id: &str,
        recipe_hash: &str,
        generation_id: &str,
        consumer_kind: CacheConsumerKind,
        consumer_id: &str,
        consumer_epoch: i64,
    ) -> Result<bool, StoreError>;

    /// Delete at most `limit` expired durable pins for one storage root,
    /// oldest first. Lookup-pin release is best effort, so crash recovery must
    /// not depend on the process that acquired a short-lived bridge pin
    /// surviving its expiry. The storage scope keeps each GC lease's database
    /// work bounded by the matching `(storage_id, expires_at_ms)` index.
    async fn prune_expired_cache_consumer_pins(
        &self,
        storage_id: &str,
        now_ms: i64,
        limit: i64,
    ) -> Result<usize, StoreError>;

    async fn shared_cache_gc_candidates(
        &self,
        storage_id: &str,
        now_ms: i64,
        limit: i64,
    ) -> Result<Vec<SharedCacheGeneration>, StoreError>;

    /// Retire the exact generation only when the supplied GC lease is still
    /// current and no live typed consumer pin exists. Filesystem deletion may
    /// happen only after this returns a fresh successor lease and the caller
    /// confirms that successor is still live.
    async fn retire_shared_cache_generation(
        &self,
        generation: &SharedCacheGeneration,
        now_ms: i64,
        lease: &Lease,
    ) -> Result<Option<Lease>, StoreError>;

    /// Remove an exact GC tombstone only after its deterministic final and
    /// quarantine paths have been removed under the same live GC lease.
    async fn finalize_retired_shared_cache_generation(
        &self,
        generation: &SharedCacheGeneration,
        now_ms: i64,
        lease: &Lease,
    ) -> Result<Option<Lease>, StoreError>;
}

/// Durable distributed work for speculative whole-title transcodes.
///
/// Candidate generation is a singleton, but execution is deliberately not:
/// every compatible node competes for rows through this boundary. Ownership
/// is a queue-row fence rather than a generic scheduler lease so a worker can
/// renew, yield, and settle independently of the next candidate pass.
#[async_trait]
pub trait PretranscodeJobStore: Send + Sync + 'static {
    /// Read one row for bounded diagnostics and lifecycle verification.
    async fn pretranscode_job(&self, id: &str) -> Result<Option<PretranscodeJob>, StoreError>;

    /// Insert one active generation unless an equivalent active/terminal job
    /// or still-verifiable ready location already satisfies it. A ready row
    /// whose last location was evicted is deliberately eligible again.
    async fn enqueue_pretranscode_job(
        &self,
        job: &NewPretranscodeJob,
        lease: &Lease,
        replacement: &Lease,
    ) -> Result<bool, StoreError>;

    /// Claim the highest-priority compatible due row. Expired running rows are
    /// eligible for takeover and advance their monotone fence.
    async fn claim_pretranscode_job(
        &self,
        node_id: &str,
        capabilities: &PretranscodeWorkerCapabilities,
        // Bounded process-local refusals (for example, sources this node
        // cannot mount). Other nodes remain eligible immediately.
        excluded_job_ids: &[String],
        now_unix_ms: i64,
        lease_expires_ms: i64,
    ) -> Result<Option<PretranscodeJob>, StoreError>;

    /// Active queue rows whose resumable part directories belong to this
    /// node. Housekeeping uses the ids as a fail-closed keep-list without
    /// publishing an incomplete cache location.
    async fn pretranscode_staging_jobs(&self, node_id: &str) -> Result<Vec<String>, StoreError>;

    /// Complete bounded active-id universe for pruning node-local source
    /// refusals. The queue schema caps active rows at 4,096.
    async fn active_pretranscode_job_ids(&self) -> Result<Vec<String>, StoreError>;

    async fn renew_pretranscode_job(
        &self,
        job: &PretranscodeJob,
        now_unix_ms: i64,
        lease_expires_ms: i64,
    ) -> Result<Option<PretranscodeJob>, StoreError>;

    /// Capacity/preemption is not a failed encode. Return the row to the due
    /// queue without incrementing attempts.
    async fn yield_pretranscode_job(
        &self,
        job: &PretranscodeJob,
        now_unix_ms: i64,
        not_before_ms: i64,
    ) -> Result<bool, StoreError>;

    /// Record one stable failure code. The fifth failure is terminal; earlier
    /// failures return to the queue at the caller's bounded backoff deadline.
    async fn fail_pretranscode_job(
        &self,
        job: &PretranscodeJob,
        error_code: &str,
        now_unix_ms: i64,
        not_before_ms: i64,
    ) -> Result<bool, StoreError>;

    /// Permanently cancel a claimed source generation that no longer exists
    /// or no longer matches its snapshotted bytes.
    async fn cancel_pretranscode_job(
        &self,
        job: &PretranscodeJob,
        error_code: &str,
        now_unix_ms: i64,
    ) -> Result<bool, StoreError>;

    /// Publish the node-local cache location and ready job state in one fenced
    /// transaction after the filesystem generation has been renamed.
    #[allow(clippy::too_many_arguments)]
    async fn complete_pretranscode_job(
        &self,
        job: &PretranscodeJob,
        recipe_hash: &str,
        recipe_version: i64,
        relative_dir: &str,
        bytes: i64,
        expected_previous_bytes: Option<i64>,
        manifest_digest: &str,
        now_unix_ms: i64,
    ) -> Result<bool, StoreError>;
}

/// Durable app-managed offline packages and their one renewable capability.
#[async_trait]
pub trait OfflinePackageStore: Send + Sync + 'static {
    /// Idempotency lookup, normalized-choice comparison, quota checks, and the
    /// insert happen under one transaction. Splitting them admits a tap-loop
    /// race even though SQLite access is mutexed per individual store call.
    async fn create_offline_package(
        &self,
        package: &NewOfflinePackage,
        max_rows_per_user: i64,
        max_bytes_per_user: i64,
        max_bytes_global: i64,
    ) -> Result<OfflineCreateOutcome, StoreError>;

    async fn offline_package_for_user(
        &self,
        package_id: &str,
        user_id: i64,
    ) -> Result<Option<OfflinePackage>, StoreError>;

    /// Authenticated status polling renews package interest and the same lease
    /// URL in place; it never rotates a platform downloader's cache identity.
    async fn renew_offline_package_for_user(
        &self,
        package_id: &str,
        user_id: i64,
        expires_at: i64,
    ) -> Result<Option<OfflinePackage>, StoreError>;

    /// Active preparation plus recently touched ready leases for the
    /// authenticated operator page. The limit is clamped by the store so an
    /// accidentally large caller value cannot turn polling into an unbounded
    /// read.
    async fn offline_activity_packages(
        &self,
        node_id: &str,
        now: i64,
        active_since: i64,
        limit: i64,
    ) -> Result<Vec<OfflineActivityPackage>, StoreError>;

    /// Current state and quota gauges. Aggregated in SQL so `/metrics` never
    /// has to load package rows or introduce labels from package data.
    async fn offline_package_stats(
        &self,
        node_id: &str,
        now: i64,
    ) -> Result<OfflinePackageStats, StoreError>;

    async fn reset_interrupted_offline_packages(&self, node_id: &str) -> Result<u64, StoreError>;

    /// Atomically turn offline production off and stale every cluster-wide
    /// preparation claim. Once this returns, no old generation may publish
    /// and no node may claim another package until the setting is enabled.
    async fn disable_offline_packages(&self) -> Result<u64, StoreError>;

    async fn claim_next_offline_package(
        &self,
        node_id: &str,
    ) -> Result<Option<OfflinePackage>, StoreError>;

    /// Node and claim generation fence the yield to the exact current worker.
    /// A re-homed package, or one reclaimed by the same node, must not be
    /// knocked back to `queued` by an earlier producer finishing its last part.
    async fn requeue_offline_package(
        &self,
        package_id: &str,
        node_id: &str,
        claim_generation: i64,
    ) -> Result<bool, StoreError>;

    /// Bind the content-addressed recipe as soon as production starts. The
    /// completed cache entry must become offline-owned before subtitle
    /// extraction and final publication can leave a gap for budget eviction.
    async fn set_offline_package_recipe(
        &self,
        package_id: &str,
        node_id: &str,
        claim_generation: i64,
        recipe_hash: &str,
    ) -> Result<bool, StoreError>;

    /// Spend recovery before any fallible alternate planning. A committed
    /// pending state can be resumed, but can never return to the primary.
    async fn begin_offline_decode_recovery(
        &self,
        package_id: &str,
        node_id: &str,
        claim_generation: i64,
        failed_recipe_hash: &str,
    ) -> Result<bool, StoreError>;

    /// Freeze the sole alternate after a pending recovery has been planned.
    async fn install_offline_decode_alternate(
        &self,
        package_id: &str,
        node_id: &str,
        claim_generation: i64,
        alternate_recipe_hash: &str,
    ) -> Result<bool, StoreError>;

    /// Consistent claim check for a cache hit. Fresh cache publication uses
    /// [`Self::complete_offline_cache_entry`] so the same predicate and cache
    /// completion are one database mutation.
    async fn offline_package_claim_is_current(
        &self,
        package_id: &str,
        node_id: &str,
        claim_generation: i64,
        recipe_hash: &str,
    ) -> Result<bool, StoreError>;

    /// Publish a new local cache generation only while the exact offline
    /// package claim and bound recipe still own it.
    async fn complete_offline_cache_entry(
        &self,
        package_id: &str,
        node_id: &str,
        claim_generation: i64,
        recipe_hash: &str,
        bytes: i64,
        manifest_digest: Option<&str>,
    ) -> Result<bool, StoreError>;

    /// Node and claim generation fence progress to the current worker so a
    /// doomed producer cannot flap the phase and percentage a later claim is
    /// reporting for the same package.
    async fn update_offline_progress(
        &self,
        package_id: &str,
        node_id: &str,
        claim_generation: i64,
        phase: &str,
        progress_millis: i64,
    ) -> Result<bool, StoreError>;

    /// Node and claim generation fence the write to the current worker.
    /// Without both, a departed owner or an earlier same-node claim could
    /// terminate work a later producer has already taken over.
    async fn fail_offline_package(
        &self,
        package_id: &str,
        node_id: &str,
        claim_generation: i64,
        phase: &str,
        code: &str,
        message: &str,
    ) -> Result<bool, StoreError>;

    /// Mark only the still-ready package bound to this corrupt recipe as
    /// failed. Request-time integrity checks use a separate exact transition
    /// so a late producer error cannot demote an unrelated ready package.
    async fn invalidate_ready_offline_package(
        &self,
        package_id: &str,
        node_id: &str,
        recipe_hash: &str,
        code: &str,
        message: &str,
    ) -> Result<bool, StoreError>;

    async fn put_offline_lease(
        &self,
        package_id: &str,
        user_id: i64,
        token_hash: &str,
        expires_at: i64,
    ) -> Result<OfflineLeaseOutcome, StoreError>;

    /// A media read both authorizes and renews the stable URL. `None` covers
    /// wrong, revoked, expired, not-ready, and missing packages deliberately.
    async fn offline_package_for_lease(
        &self,
        token_hash: &str,
        now: i64,
        renewed_expires_at: i64,
    ) -> Result<Option<OfflinePackage>, StoreError>;

    /// Node, claim generation, and recipe fence publication for the same
    /// reason [`OfflinePackageStore::fail_offline_package`] is fenced. A stale
    /// producer must not advertise bytes from a departed or superseded claim.
    async fn mark_offline_package_ready(
        &self,
        package_id: &str,
        node_id: &str,
        claim_generation: i64,
        recipe_hash: &str,
        actual_bytes: i64,
        duration_ms: i64,
    ) -> Result<bool, StoreError>;

    async fn delete_offline_package(
        &self,
        package_id: &str,
        user_id: i64,
    ) -> Result<bool, StoreError>;

    async fn expire_offline_packages(&self, now: i64) -> Result<u64, StoreError>;

    // --- Node removal (`CLUSTERING-PLAN.md` §6.7) -------------------------
    //
    // Removing a node must resolve the offline work it owns before the
    // membership change commits. A package's `source_path` replicates, but a
    // mount does not, so re-homing is only allowed against a survivor that
    // answered a probe by actually reading the snapshotted source. Everything
    // below exists to make that proof durable rather than assumed.
    //
    // The single-node SQLite backend has no removal path at all. Its
    // implementations are deliberately inert, and no SQLite table backs them.

    /// Every package the node still owns that removal has to resolve —
    /// `queued`, `preparing`, and `ready`. `failed` rows are terminal, hold no
    /// reservation, and are left for the ordinary expiry sweep.
    async fn unresolved_offline_packages(
        &self,
        node_id: &str,
    ) -> Result<Vec<OfflinePackage>, StoreError>;

    /// How many of the node's `ready` packages a client is fetching right now,
    /// measured the way the activity surface measures it. Removal refuses
    /// while a transfer is in flight rather than cutting a download off.
    async fn offline_transfers_in_flight(
        &self,
        node_id: &str,
        now: i64,
        active_since: i64,
    ) -> Result<i64, StoreError>;

    /// Ask each candidate node to prove it can read each package's source.
    /// Re-asking resets any previous answer: a mount that worked last week is
    /// not evidence about this removal.
    async fn request_offline_source_probes(
        &self,
        package_ids: &[String],
        node_ids: &[String],
        now: i64,
    ) -> Result<u64, StoreError>;

    /// Packages this node has been asked about and has not answered yet. The
    /// full package row travels because the answer requires its snapshotted
    /// path, size, and mtime — the probe row itself stores no media path.
    async fn pending_offline_source_probes(
        &self,
        node_id: &str,
        requested_since: i64,
    ) -> Result<Vec<OfflinePackage>, StoreError>;

    async fn answer_offline_source_probe(
        &self,
        package_id: &str,
        node_id: &str,
        readable: bool,
        now: i64,
    ) -> Result<bool, StoreError>;

    /// Probes asked at or after `requested_at` that nobody has answered yet.
    /// Removal waits on this reaching zero rather than on a particular node
    /// saying yes, so a unanimous "no" ends the wait immediately instead of
    /// burning the whole timeout on a package no survivor can take.
    async fn outstanding_offline_source_probes(&self, requested_at: i64)
        -> Result<i64, StoreError>;

    /// Nodes that answered "yes" to a probe asked at or after
    /// `requested_since`. Scoped by when the question was asked rather than
    /// when it was answered, because the asking node and the answering node
    /// keep different clocks and only the former's is consistent here.
    async fn verified_offline_source_nodes(
        &self,
        package_id: &str,
        requested_since: i64,
    ) -> Result<Vec<String>, StoreError>;

    /// Apply one whole removal plan in a single transaction, and report an
    /// error if any entry did not apply.
    ///
    /// The error is not a rollback, and this deliberately does not claim to be
    /// one: the transaction rolls back on a statement *error*, but an UPDATE
    /// that matches no row succeeds, so a package that moved underneath the
    /// plan leaves the rest of the plan applied. What keeps that safe is the
    /// caller, not the transaction — the membership change never commits on an
    /// error, the entries that did apply are individually correct resolutions
    /// (requeued work is claimable, failed work has released its reservation),
    /// and the operator's retry re-reads and finishes the job.
    ///
    /// Failing a package clears its reservation and completed size in the same
    /// statement: the bytes it accounted for lived on the departing node and
    /// reporting them as held would be a lie the operator cannot act on.
    async fn resolve_offline_packages_for_removal(
        &self,
        node_id: &str,
        plan: &[OfflineRemovalPlanEntry],
        now: i64,
    ) -> Result<OfflineRemovalReport, StoreError>;
}

/// Node-local playback telemetry.
///
/// Unlike catalogue and watch state, these rows describe what happened on one
/// machine and must never be submitted to Raft. The hiqlite implementation
/// therefore owns a separate versioned SQLite sidecar per voter.
#[async_trait]
pub trait PlaybackTelemetryStore: Send + Sync + 'static {
    async fn record_playback_event(&self, event: &PlaybackEvent) -> Result<i64, StoreError>;
    /// Persist retained events AND fold network-prior observations using one
    /// node-local connection lease and transaction.
    ///
    /// Both subjects live in the same node-local sidecar behind the same
    /// `Mutex<Connection>`, so a writer that batched its events and then
    /// called [`NetworkPriorStore::observe_network_prior`] once per event
    /// would take one lease for the batch and another for every event in it.
    /// The two opt-ins stay independent: retention off is an empty `events`,
    /// the prior opt-in off is an empty `observations`, and either alone
    /// still costs one lease. Returns the number of retained rows written,
    /// which is `events.len()` — folded observations update at most one prior
    /// row each and are not rows this count describes.
    async fn record_playback_batch(
        &self,
        events: &[PlaybackEvent],
        observations: &[NetworkPriorObservation],
    ) -> Result<u64, StoreError>;
    async fn prune_playback_events(&self, before_ms: i64, limit: i64) -> Result<u64, StoreError>;
    async fn playback_events(
        &self,
        query: &PlaybackEventQuery,
    ) -> Result<Vec<PlaybackEvent>, StoreError>;
}

/// Bounded node-local network history derived from playback telemetry.
///
/// Like [`PlaybackTelemetryStore`], this data must not pass through Raft: a
/// prior describes the network observed by one server node.
#[async_trait]
pub trait NetworkPriorStore: Send + Sync + 'static {
    async fn observe_network_prior(
        &self,
        observation: &NetworkPriorObservation,
    ) -> Result<NetworkPrior, StoreError>;
    async fn network_prior(
        &self,
        credential_generation: &str,
        client_class: &str,
        network_fingerprint: &str,
    ) -> Result<Option<NetworkPrior>, StoreError>;
    async fn prune_network_priors(&self, before_ms: i64, limit: i64) -> Result<u64, StoreError>;
}

/// Monotone, backend-neutral ownership leases for cluster work.
///
/// `now_unix_ms` is supplied by the coordinator so competing-node and clock
/// edge cases are reproducible in the backend contract. Callers must still
/// validate the returned fence in the same transaction as durable publication.
/// The monotone revision in a returned [`Lease`] is part of its compare-and-swap
/// identity: every successful renewal returns a replacement token, and delayed
/// operations using an older same-fence token fail rather than regressing or
/// resurrecting it.
#[async_trait]
pub trait CoordinationStore: Send + Sync + 'static {
    async fn acquire_lease(
        &self,
        resource: &str,
        owner_node_id: &str,
        now_unix_ms: i64,
        expires_at_unix_ms: i64,
    ) -> Result<LeaseClaim, StoreError>;

    async fn renew_lease(
        &self,
        lease: &Lease,
        now_unix_ms: i64,
        expires_at_unix_ms: i64,
    ) -> Result<Option<Lease>, StoreError>;

    async fn release_lease(&self, lease: &Lease, now_unix_ms: i64) -> Result<bool, StoreError>;
}

/// Durable mutations performed by singleton cluster jobs. Implementations
/// must validate the exact lease token and its expiry in the same atomic
/// transaction as the mutation.
#[async_trait]
pub trait FencedPublicationStore: Send + Sync + 'static {
    async fn add_downloaded_subtitle_fenced(
        &self,
        file_id: i64,
        track: &crate::domain::DownloadedSubtitle,
        lease: &Lease,
        replacement: &Lease,
    ) -> Result<bool, StoreError>;
    /// Apply exactly the server-generated repair plan while the library scan
    /// lease and the preview preimage are both current.
    async fn apply_identity_repair_fenced(
        &self,
        snapshot: &IdentityRepairSnapshot,
        plan: &IdentityRepairPlan,
        lease: &Lease,
        replacement: &Lease,
    ) -> Result<IdentityRepairOutcome, StoreError>;
    async fn put_setting_fenced(
        &self,
        key: &str,
        value: &str,
        lease: &Lease,
        replacement: &Lease,
    ) -> Result<(), StoreError>;
    async fn put_setting_if_absent_fenced(
        &self,
        key: &str,
        value: &str,
        lease: &Lease,
        replacement: &Lease,
    ) -> Result<bool, StoreError>;
    #[allow(clippy::too_many_arguments)]
    async fn put_setting_if_absent_if_artwork_repair_current_fenced(
        &self,
        key: &str,
        value: &str,
        expected_item_id: i64,
        repair_fence: &ArtworkRepairFence,
        lease: &Lease,
        replacement: &Lease,
    ) -> Result<bool, StoreError>;
    async fn mark_library_scanned_fenced(
        &self,
        id: i64,
        refreshed: bool,
        lease: &Lease,
        replacement: &Lease,
    ) -> Result<(), StoreError>;
    async fn insert_item_fenced(
        &self,
        item: &NewItem,
        lease: &Lease,
        replacement: &Lease,
    ) -> Result<i64, StoreError>;
    async fn apply_metadata_fenced(
        &self,
        item_id: i64,
        patch: &MetadataPatch,
        lease: &Lease,
        replacement: &Lease,
    ) -> Result<(), StoreError>;
    async fn apply_series_tmdb_hint_fenced(
        &self,
        library_id: i64,
        show_id: i64,
        tmdb_id: i64,
        lease: &Lease,
        replacement: &Lease,
    ) -> Result<SeriesHintOutcome, StoreError>;
    #[allow(clippy::too_many_arguments)]
    async fn apply_metadata_if_artwork_repair_current_fenced(
        &self,
        item_id: i64,
        patch: &MetadataPatch,
        repair_fence: &ArtworkRepairFence,
        lease: &Lease,
        replacement: &Lease,
    ) -> Result<bool, StoreError>;
    async fn apply_book_metadata_fenced(
        &self,
        item_id: i64,
        patch: &BookMetadataPatch,
        lease: &Lease,
        replacement: &Lease,
    ) -> Result<(), StoreError>;
    #[allow(clippy::too_many_arguments)]
    async fn apply_book_metadata_if_current_fenced(
        &self,
        expected: &Item,
        patch: &BookMetadataPatch,
        repair_fence: Option<&ArtworkRepairFence>,
        lease: &Lease,
        replacement: &Lease,
    ) -> Result<bool, StoreError>;
    async fn set_nfo_seeded_fenced(
        &self,
        item_id: i64,
        lease: &Lease,
        replacement: &Lease,
    ) -> Result<(), StoreError>;
    #[allow(clippy::too_many_arguments)]
    async fn upsert_file_fenced(
        &self,
        item_id: i64,
        path: &str,
        size: i64,
        mtime: i64,
        probe: &ProbeResult,
        lease: &Lease,
        replacement: &Lease,
    ) -> Result<i64, StoreError>;
    #[allow(clippy::too_many_arguments)]
    async fn ensure_library_root_fingerprint_fenced(
        &self,
        library_id: i64,
        fingerprint: &str,
        allow_establish: bool,
        lease: &Lease,
        replacement: &Lease,
    ) -> Result<RootFingerprintStatus, StoreError>;
    #[allow(clippy::too_many_arguments)]
    async fn reconcile_library_fenced(
        &self,
        library_id: i64,
        root_fingerprint: &str,
        gone_file_ids: &[i64],
        prune_limit: u64,
        lease: &Lease,
        replacement: &Lease,
    ) -> Result<ReconcileOutcome, StoreError>;
    #[allow(clippy::too_many_arguments)]
    async fn claim_cache_entry_fenced(
        &self,
        recipe_hash: &str,
        file_id: i64,
        recipe_version: i64,
        node_id: &str,
        relative_dir: &str,
        lease: &Lease,
        replacement: &Lease,
    ) -> Result<bool, StoreError>;
    async fn touch_cache_claim_fenced(
        &self,
        recipe_hash: &str,
        node_id: &str,
        lease: &Lease,
        replacement: &Lease,
    ) -> Result<(), StoreError>;
    #[allow(clippy::too_many_arguments)]
    async fn complete_cache_entry_fenced(
        &self,
        recipe_hash: &str,
        node_id: &str,
        relative_dir: &str,
        bytes: i64,
        manifest_digest: Option<&str>,
        lease: &Lease,
        replacement: &Lease,
    ) -> Result<(), StoreError>;
    async fn forget_cache_entry_fenced(
        &self,
        recipe_hash: &str,
        node_id: &str,
        storage_class: &str,
        lease: &Lease,
        replacement: &Lease,
    ) -> Result<(), StoreError>;
    async fn mark_dv_conversion_running_fenced(
        &self,
        file_id: i64,
        bytes_before: i64,
        lease: &Lease,
        replacement: &Lease,
    ) -> Result<bool, StoreError>;
    async fn mark_dv_conversion_verified_fenced(
        &self,
        file_id: i64,
        el_type: Option<&str>,
        bytes_after: i64,
        lease: &Lease,
        replacement: &Lease,
    ) -> Result<bool, StoreError>;
    #[allow(clippy::too_many_arguments)]
    async fn begin_dv_recovery_guard_fenced(
        &self,
        file_id: i64,
        guard_id: &str,
        recovery_path: &str,
        now_ms: i64,
        lease: &Lease,
        replacement: &Lease,
    ) -> Result<bool, StoreError>;
    #[allow(clippy::too_many_arguments)]
    async fn mark_dv_conversion_committed_fenced(
        &self,
        file_id: i64,
        original_path: Option<&str>,
        bytes_after: i64,
        finished_at_ms: i64,
        lease: &Lease,
        replacement: &Lease,
    ) -> Result<bool, StoreError>;
    #[allow(clippy::too_many_arguments)]
    async fn mark_dv_conversion_committed_with_guard_fenced(
        &self,
        file_id: i64,
        guard_id: &str,
        bytes_after: i64,
        finished_at_ms: i64,
        lease: &Lease,
        replacement: &Lease,
    ) -> Result<bool, StoreError>;
    #[allow(clippy::too_many_arguments)]
    async fn advance_dv_recovery_guard_fenced(
        &self,
        guard_id: &str,
        expected: DvRecoveryGuardState,
        next: DvRecoveryGuardState,
        updated_at_ms: i64,
        lease: &Lease,
        replacement: &Lease,
    ) -> Result<bool, StoreError>;
    async fn delete_dv_recovery_guard_fenced(
        &self,
        guard_id: &str,
        lease: &Lease,
        replacement: &Lease,
    ) -> Result<bool, StoreError>;
    async fn mark_dv_conversion_failed_fenced(
        &self,
        file_id: i64,
        error: &str,
        finished_at_ms: i64,
        lease: &Lease,
        replacement: &Lease,
    ) -> Result<bool, StoreError>;
}

/// Durable idempotency and routing for cluster-owned live HLS sessions.
///
/// Capability bytes stay in `session_id`; every mutation additionally fences
/// on the never-reused incarnation plus owner epoch. Implementations bound
/// client-controlled rows before inserting them.
#[async_trait]
pub trait MediaSessionStore: Send + Sync + 'static {
    #[allow(clippy::too_many_arguments)]
    async fn claim_media_session_request(
        &self,
        user_id: i64,
        request_id: &str,
        request_fingerprint: &str,
        playback_id: &str,
        incarnation_id: &str,
        now_ms: i64,
        claim_expires_at_ms: i64,
    ) -> Result<MediaSessionRequestClaim, StoreError>;

    /// Bind a Library-channel start's canonical worker recipe to its claimed
    /// durable identity before producer placement.
    async fn record_library_channel_session_recipe(
        &self,
        user_id: i64,
        request_id: &str,
        incarnation_id: &str,
        recipe_json: &str,
        now_ms: i64,
    ) -> Result<bool, StoreError>;

    async fn assign_media_session_request_owner(
        &self,
        user_id: i64,
        request_id: &str,
        incarnation_id: &str,
        owner_node_id: &str,
        now_ms: i64,
    ) -> Result<bool, StoreError>;

    async fn activate_media_session(
        &self,
        activation: &MediaSessionActivation,
    ) -> Result<Option<MediaSessionActivationOutcome>, StoreError>;

    /// Stage a successor that exists without being current.
    ///
    /// `Ok(None)` means the CAS lost, and there are exactly three ways to lose
    /// it: the playback pointer does not name
    /// `expected_predecessor_incarnation_id`, a staged successor already
    /// exists for this playback, or the user is at an admission bound. `Err`
    /// is malformed input and real database faults. Callers never see a bool.
    ///
    /// What it must not do, and what the acceptance tests assert it does not:
    /// run the supersession reap, and move
    /// `media_playback_pointers.updated_at_ms`. Both are things
    /// [`Self::activate_media_session`] does unconditionally, which is why a
    /// preparation cannot be built on it.
    ///
    /// A staged successor counts against the per-user admission cap, and —
    /// unlike an activation — nothing is discounted against it. Activation's
    /// counting queries exclude the incarnation the pointer names because that
    /// one is about to be reaped by the same transaction; a preparation reaps
    /// nothing, so discounting the predecessor would count a slot that is not
    /// being freed and admit one session past the cap. A prepared successor
    /// holds a real encoder slot for the whole preparation window; the price
    /// is that preparing costs a saturated user real headroom, which is the
    /// honest cost of the resource.
    ///
    /// A staged successor also takes its own `job_leases` row at prepare time,
    /// exactly as an activation does. Without one the row can never be renewed
    /// or taken over, so a committed successor would die at the preparation
    /// deadline with no recovery path — acceptance 7 has no answer for that
    /// phase otherwise.
    async fn prepare_media_session(
        &self,
        preparation: &crate::domain::MediaSessionPreparation,
    ) -> Result<Option<MediaSessionRoute>, StoreError>;

    /// Record what a viewer is asking for, and return the ownership that
    /// results.
    ///
    /// Idempotent on the ask rather than on the call. Recording the same
    /// digest again returns the existing row unchanged — the revision does not
    /// move — because a viewer repeating themselves has not changed their
    /// mind, and a revision that advanced on repetition would be the cadence
    /// counter this exists to replace. Recording a different digest advances
    /// the revision by exactly one.
    ///
    /// Written before an intent-changing request is reported accepted, so
    /// there is no window in which a client has been told its new selection
    /// was taken and nothing durable says so.
    async fn record_desired_selection(
        &self,
        user_id: i64,
        playback_id: &str,
        digest: &str,
        canonical_form: &str,
        now_ms: i64,
    ) -> Result<crate::domain::DesiredOwnership, StoreError>;

    /// What this playback is currently asking for, if anything ever said.
    ///
    /// `None` for a playback that predates this row or has never sent an
    /// intent-changing request. Absent is not "unchanged" and not "default":
    /// it is no evidence, and every caller has to treat it that way or an
    /// upgraded node would fence every session that started before it.
    /// Write a playback pointer the way a binary from before `desired_revision`
    /// would: naming only the columns that existed then.
    ///
    /// A test-only shape on a production trait, which is a cost worth naming.
    /// It exists because there is no other way to produce that write from this
    /// binary — every real writer fills the column inside its own statement —
    /// and a fence proved only against writes this binary can make is not
    /// proved against the writer it was built for. The alternative was to test
    /// the trigger as SQL text on one backend and assert nothing at all about
    /// the replicated one, where the same guard has to hold and where a
    /// divergence would surface during a restore.
    ///
    /// It is deliberately not a bypass: it takes no revision, so it cannot be
    /// used to write a pointer *around* the fence, only to attempt the exact
    /// write the fence exists to refuse.
    /// The ask a playback's pointer was written against, as the fence reads it.
    ///
    /// A read rather than a shape: the column is not on `MediaSessionRoute`
    /// and has no business being there — nothing serving a stream needs it —
    /// but a proof that the write chain reaches it does.
    async fn validation_playback_pointer_desired_revision(
        &self,
        user_id: i64,
        playback_id: &str,
    ) -> Result<Option<i64>, StoreError>;

    async fn validation_write_legacy_playback_pointer(
        &self,
        user_id: i64,
        playback_id: &str,
        incarnation_id: &str,
        now_ms: i64,
    ) -> Result<(), StoreError>;

    async fn desired_selection(
        &self,
        user_id: i64,
        playback_id: &str,
    ) -> Result<Option<crate::domain::DesiredOwnership>, StoreError>;

    /// Replace one named staged successor with a merged preparation.
    ///
    /// The abort and replacement prepare are one durable mutation. They leave
    /// the playback pointer and its current generation untouched; only the
    /// ordinary preparation commit may advance that pointer. This is the
    /// occupied-slot operation used when subtitle burn work joins successor
    /// work that is already in flight.
    ///
    /// `Ok(Some(route))` means the merged preparation now owns the slot. That
    /// includes an exact retry after the first rejoin succeeded but its owner
    /// crashed before observing the response. `Ok(None)` means the named row
    /// is gone and the caller's own merged preparation is not the staged row;
    /// the caller must re-read the current generation before deriving another
    /// preparation. The merged preparation's expected predecessor must equal
    /// the predecessor recorded by the named ledger row: a pointer advance
    /// never retargets an occupied slot. If that identity differs, or the
    /// named row still owns the slot but the replacement no longer satisfies
    /// prepare admission, the transaction leaves the row intact and returns
    /// `Err`; silently dropping occupied work is not a CAS loss. Malformed
    /// identities and database faults are also `Err`.
    async fn rejoin_media_session_preparation(
        &self,
        staged_incarnation_id: &str,
        preparation: &crate::domain::MediaSessionPreparation,
    ) -> Result<Option<MediaSessionRoute>, StoreError>;

    /// The staged successor for one playback, if there is one.
    async fn staged_media_session_for_playback(
        &self,
        user_id: i64,
        playback_id: &str,
    ) -> Result<Option<crate::domain::MediaSessionStagedGeneration>, StoreError>;

    /// Make the staged successor current, in one durable mutation.
    ///
    /// Advances the pointer from the preparation's recorded predecessor to the
    /// staged incarnation, retires that exact predecessor, and clears the
    /// ledger row.
    ///
    /// `Ok(None)` when the pointer no longer names the recorded predecessor.
    /// That case is the whole reason the predecessor is recorded at
    /// preparation time rather than re-read here: a pointer that moved means a
    /// newer player generation exists, and the correct outcome is to **abort
    /// the staged generation, not reap the newer one**. A commit that read the
    /// pointer fresh would do the opposite and would look correct doing it.
    ///
    /// `lease_expires_at_ms` is the successor's boundary as a *serving*
    /// session, and commit is where it has to be supplied. Until now the row
    /// carried the preparation deadline, which is a much shorter clock chosen
    /// for a candidate nobody is watching; a successor promoted without a new
    /// boundary would be ended by maintenance at the moment the preparation
    /// would have expired, taking the playback's pointer with it.
    ///
    /// The request also fences the predecessor on the owner node/epoch the
    /// actor accepted and requires the ledger deadline to remain live in the
    /// pointer transaction. An optional control receipt is stored atomically
    /// with a successful advance for replay after the predecessor retires.
    async fn commit_media_session_preparation(
        &self,
        user_id: i64,
        playback_id: &str,
        request: &crate::domain::MediaSessionPreparationCommitRequest,
    ) -> Result<Option<crate::domain::MediaSessionPreparationCommit>, StoreError>;

    /// Discard the staged successor and leave the current stream authoritative.
    ///
    /// Ends the staged row as `replaced` and clears the ledger row. `replaced`
    /// rather than a new `terminal_reason` value: the CHECK constraint is an
    /// enum and widening it on SQLite is a table rebuild, and an abandoned
    /// successor really was replaced — by the predecessor it never displaced.
    ///
    /// The outcome is a re-read plus a predicate, never `rows_affected`: an
    /// owner retrying an abort after a crash reads back the same ended route
    /// rather than a spurious loss. `Ok(None)` means the named incarnation is
    /// not an aborted successor of this playback — it was never staged, or it
    /// committed and is now current.
    /// The predecessor owner tuple is part of the same mutation, so an old
    /// owner's timer cannot tear down a successor after takeover.
    async fn abort_media_session_preparation(
        &self,
        user_id: i64,
        playback_id: &str,
        request: &crate::domain::MediaSessionPreparationAbortRequest,
    ) -> Result<Option<MediaSessionRoute>, StoreError>;

    /// Take this epoch's one automatic decoder recovery, if it has not been
    /// taken.
    ///
    /// `Ok(None)` means the budget is gone: either another identity reserved it
    /// first, or this playback already spent it. It is deliberately not
    /// distinguishable from "spent by me on a different failure" at this
    /// signature, because the caller's response is the same either way — do not
    /// install an alternative.
    ///
    /// An exact replay of the same request returns the existing reservation, so
    /// an executor that crashed between reserving and spawning can find out
    /// what it had already decided. A request naming a *different* failure —
    /// another attempt, another decision, another plan, or a different decode
    /// restriction — is not a replay and gets `Ok(None)`: that would be a
    /// second recovery wearing the first one's identity. The restriction is
    /// part of that comparison because the conditional insert does not update
    /// an existing row, so returning `Some` for a request whose restriction was
    /// never persisted would hand the caller a decision the store does not
    /// hold.
    ///
    /// The budget is consumed at reservation and never refunded. Not by
    /// timeout, not by cancellation, not by a crash, not by a failed spawn.
    /// That direction is chosen on purpose: an unbounded retry loop against a
    /// decoder that cannot decode the file is worse for the viewer than one
    /// lost attempt, and every refund path is a way for the loop to come back.
    ///
    /// Implementations must reserve with a single conditional insert. A read
    /// followed by an insert is a race that grants two nodes the same budget,
    /// and a Hiqlite affected-row result cannot be rolled back afterwards.
    async fn reserve_producer_recovery(
        &self,
        request: &crate::domain::ProducerRecoveryRequest,
        now_ms: i64,
    ) -> Result<Option<crate::domain::ProducerRecoveryReservation>, StoreError>;

    /// Record what became of a reserved recovery.
    ///
    /// `Installed` or `Exhausted` only, and only from `Reserved`: the states do
    /// not cycle, because a state that can return to `Reserved` is a refund
    /// with extra steps.
    ///
    /// Fenced on `failed_incarnation_id`, so only the identity that reserved
    /// can settle. Without it, anything holding the three key fields — a stale
    /// node that handled this playback before a handoff, say — could exhaust a
    /// live reservation, and the owner's own later settle would find nothing to
    /// update and read back somebody else's terminal state.
    ///
    /// Idempotent for the same terminal state: an owner that crashed after
    /// settling and settles again gets its reservation back rather than a
    /// silence it cannot tell from a loss. `Ok(None)` means this identity has
    /// no reservation in that state — never reserved, already settled the
    /// *other* way, or not the reserver.
    ///
    /// The restriction is retained through settlement in both directions. An
    /// exhausted recovery still knows which decoder failed, and every later
    /// continuation still has to avoid it.
    async fn settle_producer_recovery(
        &self,
        user_id: i64,
        playback_id: &str,
        recovery_epoch: &str,
        failed_incarnation_id: &str,
        state: crate::domain::ProducerRecoveryState,
        now_ms: i64,
    ) -> Result<Option<crate::domain::ProducerRecoveryReservation>, StoreError>;

    /// What this epoch has already decided, for the paths that must not
    /// re-plan onto a decoder that already failed.
    ///
    /// Read by every ordinary continuation — seek, track change, output
    /// change, automatic reopen, ownership handoff — before resolving a plan
    /// or looking up a cached one.
    async fn producer_recovery_for_epoch(
        &self,
        user_id: i64,
        playback_id: &str,
        recovery_epoch: &str,
    ) -> Result<Option<crate::domain::ProducerRecoveryReservation>, StoreError>;

    /// Validation only: replace one ledger row's stored restriction with an
    /// arbitrary string, bypassing every check this store makes on the way in.
    ///
    /// It is on the trait rather than on one backend because the property it
    /// exists to prove is a *dual-backend* property: a restriction that a later
    /// build wrote and this one cannot read must be a refusal on both stores
    /// and the same kind of refusal, and that state is unreachable through the
    /// ordinary API by construction — everything this build writes, it can read
    /// back. A rule only one backend enforces is precisely the failure this
    /// store's contract suite exists to catch.
    ///
    /// `Ok(false)` when there is no such row.
    async fn validation_corrupt_recovery_restriction(
        &self,
        user_id: i64,
        playback_id: &str,
        recovery_epoch: &str,
        stored: &str,
    ) -> Result<bool, StoreError>;

    async fn settle_media_session_activation(
        &self,
        activation: &MediaSessionActivation,
        settlement: MediaSessionActivationSettlement,
        now_ms: i64,
    ) -> Result<Option<MediaSessionRoute>, StoreError>;

    /// Publish an exact prepared activation after its owner has observed the
    /// serving result. The request remains in-flight until this CAS; replaying
    /// the same resolved request returns its durable route.
    async fn publish_media_session_activation(
        &self,
        user_id: i64,
        request_id: &str,
        incarnation_id: &str,
        now_ms: i64,
    ) -> Result<Option<MediaSessionRoute>, StoreError>;

    /// Replace the durable unobserved sentinel with one full, freshly minted
    /// not-before boundary. Exact ownership changes and terminal state fail
    /// closed. A concurrent acknowledgement may return an already-ready row.
    async fn arm_media_session_handoff(
        &self,
        incarnation_id: &str,
        owner_node_id: &str,
        owner_epoch: i64,
        publication_ready_at_ms: i64,
        now_ms: i64,
    ) -> Result<Option<MediaSessionRoute>, StoreError>;

    /// Clear one successor's durable publication fence with an exact
    /// acknowledgement or exact armed-boundary proof. Ownership changes,
    /// terminal state, and unrelated incarnations fail closed.
    async fn complete_media_session_handoff(
        &self,
        incarnation_id: &str,
        owner_node_id: &str,
        owner_epoch: i64,
        proof: MediaSessionProjectionCompletion,
        now_ms: i64,
    ) -> Result<Option<MediaSessionRoute>, StoreError>;

    /// Arm the replay-visible fallback for an exact terminal owner after a
    /// definitive post-End observation. Ended routes reuse the publication
    /// column as terminal-projection state because they can no longer serve.
    async fn arm_media_session_terminal_projection(
        &self,
        incarnation_id: &str,
        owner_node_id: &str,
        owner_epoch: i64,
        projection_safe_at_ms: i64,
        now_ms: i64,
    ) -> Result<Option<MediaSessionRoute>, StoreError>;

    /// Persist exact owner acknowledgement or an exact armed-boundary proof
    /// so a later idempotent release does not restart its safety interval.
    async fn complete_media_session_terminal_projection(
        &self,
        incarnation_id: &str,
        owner_node_id: &str,
        owner_epoch: i64,
        proof: MediaSessionProjectionCompletion,
        now_ms: i64,
    ) -> Result<Option<MediaSessionRoute>, StoreError>;

    async fn fail_media_session_request(
        &self,
        user_id: i64,
        request_id: &str,
        incarnation_id: &str,
        now_ms: i64,
    ) -> Result<bool, StoreError>;

    async fn media_session_route(
        &self,
        session_id: &str,
    ) -> Result<Option<MediaSessionRoute>, StoreError>;

    async fn media_session_route_by_incarnation(
        &self,
        incarnation_id: &str,
    ) -> Result<Option<MediaSessionRoute>, StoreError>;

    /// Resolve the exact route currently named by one player's durable
    /// pointer. Activation uses this read as the predecessor half of its CAS
    /// so commit-unknown reconciliation retains the identity it must fence.
    async fn media_session_route_for_playback(
        &self,
        user_id: i64,
        playback_id: &str,
    ) -> Result<Option<MediaSessionRoute>, StoreError>;

    /// Atomically store the first exact terminal-control acknowledgement and
    /// fence that exact owner route as ended. A conflicting identity/sequence
    /// cannot replace the winner or end the route; repeating the identical
    /// write is idempotent.
    async fn record_media_session_terminal_ack(
        &self,
        acknowledgement: &crate::domain::MediaSessionTerminalAck,
    ) -> Result<bool, StoreError>;

    /// Read an unexpired terminal acknowledgement for an exact capability.
    async fn media_session_terminal_ack(
        &self,
        session_id: &str,
        now_ms: i64,
    ) -> Result<Option<crate::domain::MediaSessionTerminalAck>, StoreError>;

    async fn renew_media_sessions(
        &self,
        owner_node_id: &str,
        renewals: &[MediaSessionRenewal],
        now_ms: i64,
        lease_expires_at_ms: i64,
    ) -> Result<Vec<String>, StoreError>;

    /// Read a bounded, oldest-first inventory of expired active routes that a
    /// survivor may independently prove it can reproduce. `after` is an
    /// exclusive keyset cursor in `(lease_expires_at_ms, incarnation_id)`
    /// order, allowing a bounded caller to make progress past routes it must
    /// refuse without mutating those still-authoritative routes.
    async fn expired_media_sessions(
        &self,
        now_ms: i64,
        after: Option<crate::domain::MediaSessionTakeoverCursor>,
        limit: usize,
    ) -> Result<Vec<MediaSessionRoute>, StoreError>;

    /// Atomically transfer an exact expired owner epoch and its lease fence.
    /// A racing survivor, delete, renewal, or maintenance pass makes the CAS
    /// return `None` rather than publishing a second owner.
    async fn claim_media_session_takeover(
        &self,
        takeover: &MediaSessionTakeover,
    ) -> Result<Option<MediaSessionRoute>, StoreError>;

    /// End only the exact incarnation/owner epoch/lease boundary named by
    /// `end`. A same-epoch renewal after the caller's read defeats the CAS.
    ///
    /// This is the lease-loss counterpart to takeover's CAS.  It is
    /// idempotent for an already-ended matching incarnation and returns
    /// `None` when ownership advanced before cleanup reached the Store.
    async fn end_media_session_if_owner(
        &self,
        end: &crate::domain::MediaSessionEnd,
    ) -> Result<Option<MediaSessionRoute>, StoreError>;

    async fn end_media_session(
        &self,
        session_id: &str,
        terminal_reason: &str,
        now_ms: i64,
    ) -> Result<Option<MediaSessionRoute>, StoreError>;

    async fn maintain_media_sessions(&self, now_ms: i64) -> Result<(), StoreError>;

    async fn owned_media_sessions(
        &self,
        owner_node_id: &str,
        now_ms: i64,
    ) -> Result<Vec<OwnedMediaSessionLease>, StoreError>;
}

/// Node-local fragment indexes.
///
/// Like [`PlaybackTelemetryStore`], these rows describe what one machine's
/// ffmpeg produced from one machine's copy of a file, and must never be
/// submitted to Raft: an index is a list of output byte counts, and
/// [`crate::segplan::match_landing`] compares exactly those numbers to decide
/// where a repositioned producer landed. One node's answer governing another
/// node's bytes would put a viewer in the wrong part of the film with nothing
/// to report it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FragmentIndexValidationBackfill {
    pub validated: u64,
    pub refused: u64,
    pub gone: u64,
    pub remaining: u64,
}

#[async_trait]
pub trait FragmentIndexStore: Send + Sync + 'static {
    /// Store or replace one file's index.
    async fn put_fragment_index(
        &self,
        file_id: i64,
        index: &crate::segplan::FragmentIndex,
    ) -> Result<(), StoreError>;

    /// The stored index, but only when it still describes this source.
    ///
    /// Invalidation is by mismatch rather than by deletion: a changed file or
    /// a changed video pipeline simply stops matching, so nothing has to
    /// notice the change and nothing can fail to.
    async fn fragment_index(
        &self,
        file_id: i64,
        identity: &crate::segplan::SourceIdentity,
    ) -> Result<Option<crate::segplan::FragmentIndex>, StoreError>;

    /// Validate one bounded page of legacy rows in this node's local store.
    /// The pass marks structural refusals but never deletes an index.
    async fn validate_fragment_index_page(
        &self,
        limit: u32,
    ) -> Result<FragmentIndexValidationBackfill, StoreError>;

    /// Drop one file's index. `true` when a row was there.
    async fn forget_fragment_index(&self, file_id: i64) -> Result<bool, StoreError>;

    /// Whether this node's own index table holds any index — for any video
    /// pipeline — for the file's source at `source_size`/`source_mtime`.
    ///
    /// Node-local, like every row here. The subtitle-source store reads it to
    /// say why a lookup found no directory: an index built on this node means
    /// the pass that keeps PGS tracks ran here.
    async fn holds_fragment_index_for_source(
        &self,
        file_id: i64,
        source_size: i64,
        source_mtime: i64,
    ) -> Result<bool, StoreError>;

    /// Record that this identity could not be indexed, and answer when it may
    /// be attempted again.
    ///
    /// The counterpart of [`FragmentIndexStore::put_fragment_index`], and the
    /// reason it exists is that a refusal used to be a log line: the
    /// background pass logged "fragment index incomplete", moved its cursor
    /// on, and spent the same whole-file read again on the next wrap of the
    /// library — while every session for that title answered
    /// `vod_index_pending` with nothing anywhere saying why.
    async fn record_fragment_index_outcome(
        &self,
        file_id: i64,
        source: &crate::segplan::SourceIdentity,
        refusal: crate::segplan::IndexRefusal,
        reason: &str,
    ) -> Result<crate::segplan::FragmentIndexOutcome, StoreError>;

    #[allow(clippy::too_many_arguments)]
    async fn record_fragment_index_typed_outcome(
        &self,
        file_id: i64,
        source: &crate::segplan::SourceIdentity,
        code: crate::content_analysis::IndexFailureCode,
        transient_allowlisted: bool,
        reason: &str,
        rows: u32,
        diagnostic: &crate::content_analysis::IndexDiagnostic,
        max_attempts: u32,
    ) -> Result<crate::segplan::FragmentIndexOutcome, StoreError>;

    /// The recorded refusal for this identity, if it still describes this
    /// source. Invalidated by mismatch, exactly like the index itself.
    async fn fragment_index_outcome(
        &self,
        file_id: i64,
        identity: &crate::segplan::SourceIdentity,
    ) -> Result<Option<crate::segplan::FragmentIndexOutcome>, StoreError>;

    /// File ids this node holds node-local VOD rows for — indexes, plans, or
    /// both. Bounded, and ordered so a sweep makes progress across ticks.
    ///
    /// The sweep this feeds cannot be a hook inside `delete_files`. That is a
    /// *replicated* write, while these rows live in each node's own sidecar
    /// and share no transaction with it — so a hook there would only ever
    /// clean the node that happened to run the delete, and would miss every
    /// node that was down at the time. Asking each node what it holds and
    /// checking those ids against the replicated `files` table converges
    /// everywhere, on each node's own schedule.
    async fn vod_row_file_ids(&self, limit: i64) -> Result<Vec<i64>, StoreError>;

    /// Which of `file_ids` still exist in the replicated `files` table.
    async fn surviving_file_ids(&self, file_ids: &[i64]) -> Result<Vec<i64>, StoreError>;
}

/// Node-local rendition plans.
///
/// Node-local for [`FragmentIndexStore`]'s reason, and persisted for a
/// different one. An index is a measurement: lose it and it is rebuilt from
/// the file. A plan is a *decision* — ledger D10 makes it normative, and a
/// client holds a playlist naming its boundaries by index. Re-deriving one is
/// not a cheaper way to the same answer, because [`crate::fmp4::CutPolicy`] is
/// built from tuning constants a release may change while the file, the
/// pipeline and the index all stay identical. See
/// [`crate::store::renditionplan`].
#[async_trait]
pub trait RenditionPlanStore: Send + Sync + 'static {
    /// Store a plan unless the key already has one. `true` when this call is
    /// the one that stored it.
    ///
    /// Not an upsert, and that is the point: the first plan written under a
    /// key is the plan. A rendition whose boundaries must genuinely differ
    /// gets a different key.
    async fn put_rendition_plan(
        &self,
        rendition_key: &str,
        file_id: i64,
        plan: &crate::segplan::SegmentPlan,
        source: &crate::segplan::SourceIdentity,
    ) -> Result<bool, StoreError>;

    /// The stored plan, but only when it still describes this source.
    async fn rendition_plan(
        &self,
        rendition_key: &str,
        source: &crate::segplan::SourceIdentity,
    ) -> Result<Option<crate::segplan::SegmentPlan>, StoreError>;

    /// Drop exactly one rendition plan by its immutable rendition key.
    /// Answers whether a row existed. Process-generation cache reconciliation
    /// uses this instead of deleting every valid copy/current plan for a file.
    async fn forget_rendition_plan(&self, rendition_key: &str) -> Result<bool, StoreError>;

    /// Drop every rendition plan for a file. Answers how many went.
    async fn forget_rendition_plans(&self, file_id: i64) -> Result<usize, StoreError>;
}

/// Replicated semantic timeline annotations.
///
/// Unlike [`FragmentIndexStore`], these rows are small, correctable decisions
/// and therefore belong in the authority store rather than a node sidecar.
#[async_trait]
pub trait TimelineAnnotationStore: Send + Sync + 'static {
    /// Replace the automatic annotation set after validating its complete
    /// source identity and timeline bounds.
    async fn put_timeline_annotation_set(
        &self,
        file_id: i64,
        duration_ms: i64,
        set: &crate::segplan::TimelineAnnotationSet,
    ) -> Result<(), StoreError>;

    /// Publish a request-path fallback without overwriting a generation that
    /// another worker published for the same source while the probe ran. A
    /// stale row for a different physical or detector identity may be replaced.
    async fn put_timeline_annotation_set_if_missing(
        &self,
        file_id: i64,
        duration_ms: i64,
        set: &crate::segplan::TimelineAnnotationSet,
    ) -> Result<bool, StoreError>;

    /// Return the set only when it describes the caller's current source.
    async fn timeline_annotation_set(
        &self,
        file_id: i64,
        source: &crate::segplan::SourceIdentity,
    ) -> Result<Option<crate::segplan::TimelineAnnotationSet>, StoreError>;

    async fn forget_timeline_annotation_set(&self, file_id: i64) -> Result<bool, StoreError>;

    /// Set or correct one administrator-owned boundary. The store assigns the
    /// monotonic revision; the caller-supplied annotation is validated but its
    /// placeholder revision is not trusted.
    async fn set_manual_timeline_annotation(
        &self,
        file_id: i64,
        duration_ms: i64,
        source: &crate::segplan::SourceIdentity,
        annotation: &crate::segplan::TimelineAnnotation,
        generation_id: &str,
    ) -> Result<u64, StoreError>;

    /// Discard exactly the separately confirmed revision. A stale UI cannot
    /// delete a correction made after it loaded the item.
    async fn discard_manual_timeline_annotation(
        &self,
        file_id: i64,
        source: &crate::segplan::SourceIdentity,
        kind: crate::segplan::AnnotationKind,
        expected_revision: u64,
    ) -> Result<bool, StoreError>;
}

/// The full storage boundary — what plurxd holds as `Arc<dyn Store>`.
pub trait Store:
    SettingsStore
    + BackgroundJobStore
    + DvConversionStore
    + MetricsStore
    + UserStore
    + ApiKeyStore
    + LibraryStore
    + LibraryChannelStore
    + DvrStore
    + MediaStore
    + ClassificationStore
    + WatchStore
    + ReadingStore
    + TraktStore
    + WatchedOutboxStore
    + TranscodeCacheStore
    + SharedCacheStore
    + PretranscodeJobStore
    + OfflinePackageStore
    + FileGrantStore
    + PlaybackTelemetryStore
    + NetworkPriorStore
    + FragmentIndexStore
    + ClusterFragmentIndexStore
    + RenditionPlanStore
    + TimelineAnnotationStore
    + CoordinationStore
    + FencedPublicationStore
    + MediaSessionStore
    + Send
    + Sync
    + 'static
{
}

impl<T> Store for T where
    T: SettingsStore
        + BackgroundJobStore
        + DvConversionStore
        + MetricsStore
        + UserStore
        + ApiKeyStore
        + LibraryStore
        + LibraryChannelStore
        + DvrStore
        + MediaStore
        + ClassificationStore
        + ClassificationStore
        + WatchStore
        + ReadingStore
        + TraktStore
        + WatchedOutboxStore
        + TranscodeCacheStore
        + SharedCacheStore
        + PretranscodeJobStore
        + OfflinePackageStore
        + FileGrantStore
        + PlaybackTelemetryStore
        + NetworkPriorStore
        + FragmentIndexStore
        + ClusterFragmentIndexStore
        + RenditionPlanStore
        + TimelineAnnotationStore
        + CoordinationStore
        + FencedPublicationStore
        + MediaSessionStore
        + Send
        + Sync
        + 'static
{
}

/// Reopen the immutable fragment-index generation selected by the daemon
/// after every advertised holder failed to supply a valid blob.
///
/// This named boundary is the exact transition used by the VOD no-holder
/// arm. Keeping it in `plurx-core` lets the backend-neutral contract execute
/// that production transition against both SQLite and the three-voter store,
/// instead of testing only the lower-level primitive and assuming the daemon
/// calls it the same way.
pub async fn requeue_cluster_fragment_index_after_no_holder(
    store: &dyn Store,
    replacement: &NewClusterFragmentIndexJob,
) -> Result<bool, StoreError> {
    store.requeue_cluster_fragment_index(replacement).await
}

/// Per-request replicated Store operation counts populated by `TimedClient`.
///
/// The HTTP daemon installs one of these around a matched request. Background
/// work and SQLite requests have no scope, so recording remains a cheap no-op.
/// Keeping the scope here, at the Store boundary, prevents route handlers from
/// having to guess how many physical local, authority, or write operations a
/// high-level Store method performed.
#[derive(Clone, Default)]
pub struct HttpStoreOperationCounts {
    counts: std::sync::Arc<[std::sync::atomic::AtomicU64; 3]>,
    watch_reads: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl HttpStoreOperationCounts {
    #[must_use]
    pub fn snapshot(&self) -> [u64; 3] {
        use std::sync::atomic::Ordering;

        std::array::from_fn(|index| self.counts[index].load(Ordering::Relaxed))
    }

    /// Watch-state reads the Store performed inside this request (K-04 M3),
    /// on either backend: one per statement a [`WatchStore`] read method
    /// sends to the replicated store, Authority or local, and one per
    /// connection checkout on SQLite. Catalogue queries that merely filter by
    /// watch state are catalogue reads and are not counted here.
    #[must_use]
    pub fn watch_reads(&self) -> u64 {
        self.watch_reads.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn record(&self, class_index: usize) {
        use std::sync::atomic::Ordering;

        let _ = self.counts[class_index].fetch_update(
            Ordering::Relaxed,
            Ordering::Relaxed,
            |current| (current != u64::MAX).then(|| current.saturating_add(1)),
        );
    }
}

tokio::task_local! {
    static HTTP_STORE_OPERATION_COUNTS: HttpStoreOperationCounts;
}

/// Scope one HTTP request so replicated Store operations can be attributed
/// after its response is ready without putting route labels in `plurx-core`.
pub async fn scope_http_store_operations<T>(
    counts: HttpStoreOperationCounts,
    future: impl std::future::Future<Output = T>,
) -> T {
    HTTP_STORE_OPERATION_COUNTS.scope(counts, future).await
}

pub(super) fn record_http_store_operation(class_index: usize) {
    let _ = HTTP_STORE_OPERATION_COUNTS.try_with(|counts| counts.record(class_index));
}

/// Count one watch-state read into the current request, if any. Called by
/// both backends at the one place each issues a watch read.
pub(super) fn record_http_watch_read() {
    let _ = HTTP_STORE_OPERATION_COUNTS.try_with(|counts| {
        counts
            .watch_reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    });
}

/// Both halves of [`WatchStore::watch_summary`].
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub struct WatchSummary {
    /// [`WatchStore::watch_map`]'s rows for the requested items.
    pub watch: Vec<(i64, WatchState)>,
    /// [`WatchStore::watch_rollups`]'s answer: every requested container,
    /// `0/0` when nothing playable sits under it.
    pub rollups: std::collections::HashMap<i64, WatchRollup>,
}

/// Both rails of [`WatchStore::progress_rails`].
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct ProgressRails {
    pub continue_watching: Vec<InProgressItem>,
    pub next_up: Vec<RecentItem>,
}

/// The watch-state commit position acknowledged inside one HTTP request
/// (K-04 M2), offered to the client as `X-Plurx-Commit-Index`.
///
/// Only a request whose every watch write reported its Raft log index has a
/// position to offer: echoing the index of one write while another in the
/// same request is unknown would let a peer serve a read that misses the
/// unknown one. Such a request says so instead ([`CommitIndexOffer::Unknown`]),
/// because silence would leave the client echoing the index of its previous
/// write, which is older than the one it was just told succeeded.
#[derive(Clone, Default)]
pub struct HttpWatchWriteAck {
    state: std::sync::Arc<HttpWatchWriteAckState>,
}

#[derive(Default)]
struct HttpWatchWriteAckState {
    max_index: std::sync::atomic::AtomicU64,
    unprovable: std::sync::atomic::AtomicBool,
}

/// What one response says about the watch writes its request made, as
/// `X-Plurx-Commit-Index` (K-04 M2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommitIndexOffer {
    /// Every watch write in the request reported its Raft log index; this is
    /// the highest. The client echoes it as `X-Plurx-Read-After`.
    Index(u64),
    /// At least one watch write's index is unknown: an older leader or a
    /// proxy answered it, or it failed and may still commit. The client must
    /// drop the index it holds, so its next reads go to Authority.
    Unknown,
}

impl HttpWatchWriteAck {
    /// The index to offer, if every watch write in the request reported one.
    #[must_use]
    pub fn commit_index(&self) -> Option<u64> {
        match self.offer() {
            Some(CommitIndexOffer::Index(index)) => Some(index),
            Some(CommitIndexOffer::Unknown) | None => None,
        }
    }

    /// What the response says: nothing when the request made no watch
    /// write, the highest index when every one reported it, and
    /// [`CommitIndexOffer::Unknown`] otherwise.
    #[must_use]
    pub fn offer(&self) -> Option<CommitIndexOffer> {
        use std::sync::atomic::Ordering;

        if self.state.unprovable.load(Ordering::Acquire) {
            return Some(CommitIndexOffer::Unknown);
        }
        let index = self.state.max_index.load(Ordering::Acquire);
        (index > 0).then_some(CommitIndexOffer::Index(index))
    }

    /// Record one acknowledged watch write. Exposed so HTTP-layer tests can
    /// drive the response header without a replicated store.
    pub fn record(&self, log_index: Option<u64>) {
        use std::sync::atomic::Ordering;

        match log_index {
            Some(index) => {
                self.state.max_index.fetch_max(index, Ordering::AcqRel);
            }
            None => self.state.unprovable.store(true, Ordering::Release),
        }
    }
}

tokio::task_local! {
    static HTTP_WATCH_WRITE_ACK: HttpWatchWriteAck;
}

/// Scope one HTTP request so the watch writes it acknowledges can be offered
/// back to the client after its response is ready.
pub async fn scope_http_watch_write_ack<T>(
    ack: HttpWatchWriteAck,
    future: impl std::future::Future<Output = T>,
) -> T {
    HTTP_WATCH_WRITE_ACK.scope(ack, future).await
}

/// Record a watch write into the current request's acknowledgement, if the
/// write runs inside one. Background writers (the progress coalescer's
/// trailing flush, Trakt sync) have no request and record nothing here; the
/// per-process fence still covers them.
#[cfg_attr(not(feature = "hiqlite-store"), allow(dead_code))]
pub(crate) fn record_http_watch_write(log_index: Option<u64>) {
    let _ = HTTP_WATCH_WRITE_ACK.try_with(|ack| ack.record(log_index));
}

/// Record a watch write into the current request exactly as the replicated
/// store does, for HTTP-layer contracts that run without one.
#[doc(hidden)]
pub fn validation_record_http_watch_write(log_index: Option<u64>) {
    record_http_watch_write(log_index);
}

/// The only application-facing boundary for catalogue consistency choices.
///
/// Ordinary [`Store`] methods remain Authority. This wrapper may run one
/// explicitly eligible Hiqlite catalogue operation locally, but only while a
/// fresh quorum watermark, matching local Raft generation, negotiated read
/// protocol, and configured apply-lag budget all remain valid. It revalidates
/// after the complete operation and discards the result before falling back to
/// Authority when the proof changes in flight.
#[derive(Clone)]
pub struct CatalogueReader {
    authority: Arc<dyn Store>,
    #[cfg(feature = "hiqlite-store")]
    bounded: Option<BoundedCatalogueReader>,
}

#[cfg(feature = "hiqlite-store")]
#[derive(Clone)]
struct BoundedCatalogueReader {
    store: Arc<HiqliteAuthStore>,
    metrics: crate::cluster::migration::status::PassiveRaftMetrics,
    enabled: bool,
    max_apply_lag_entries: u64,
    #[cfg(feature = "cluster-read-cost-validation")]
    revoke_after_next_local: Arc<std::sync::atomic::AtomicBool>,
}

impl CatalogueReader {
    /// Preserve the existing Authority behavior for SQLite, recovery boots,
    /// tests, and callers that have not opted into bounded replica reads.
    #[must_use]
    pub fn authority(store: Arc<dyn Store>) -> Self {
        Self {
            authority: store,
            #[cfg(feature = "hiqlite-store")]
            bounded: None,
        }
    }

    #[cfg(feature = "hiqlite-store")]
    pub(crate) fn replicated(
        authority: Arc<dyn Store>,
        store: Arc<HiqliteAuthStore>,
        metrics: crate::cluster::migration::status::PassiveRaftMetrics,
        enabled: bool,
        max_apply_lag_entries: u64,
    ) -> Self {
        Self {
            authority,
            bounded: Some(BoundedCatalogueReader {
                store,
                metrics,
                enabled,
                max_apply_lag_entries,
                #[cfg(feature = "cluster-read-cost-validation")]
                revoke_after_next_local: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            }),
        }
    }

    /// Construct the production bounded reader for real seeded-store
    /// contracts outside this module.
    #[cfg(all(feature = "hiqlite-store", feature = "cluster-read-cost-validation"))]
    #[doc(hidden)]
    #[must_use]
    pub fn validation_replicated(
        authority: Arc<dyn Store>,
        store: Arc<HiqliteAuthStore>,
        metrics: crate::cluster::migration::status::PassiveRaftMetrics,
        max_apply_lag_entries: u64,
    ) -> Self {
        Self::replicated(authority, store, metrics, true, max_apply_lag_entries)
    }

    /// Revoke the proof after exactly the next local query and before its
    /// post-query validation, forcing result discard and Authority fallback.
    #[cfg(all(feature = "hiqlite-store", feature = "cluster-read-cost-validation"))]
    #[doc(hidden)]
    pub fn validation_revoke_after_next_local(&self) {
        if let Some(bounded) = &self.bounded {
            bounded
                .revoke_after_next_local
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    #[cfg(feature = "hiqlite-store")]
    async fn bounded<T, F, Fut>(&self, local_read: F) -> Option<T>
    where
        F: FnOnce(Arc<HiqliteAuthStore>) -> Fut,
        Fut: std::future::Future<Output = Result<T, StoreError>>,
    {
        self.bounded_after(|_| Some(0), local_read).await
    }

    /// A bounded read of per-user watch state (K-04 M2). On top of the
    /// catalogue permit, the local replica must have applied the client's
    /// echoed `read_after`, raised to the latest watch write this process
    /// acknowledged for the user when that is newer. Without `read_after`
    /// Authority answers, whatever this process recorded: its own older
    /// write proves nothing about a later one through a peer. See
    /// [`watch_fence`].
    #[cfg(feature = "hiqlite-store")]
    async fn bounded_watch<T, F, Fut>(
        &self,
        user_id: i64,
        read_after: Option<u64>,
        local_read: F,
    ) -> Option<T>
    where
        F: FnOnce(Arc<HiqliteAuthStore>) -> Fut,
        Fut: std::future::Future<Output = Result<T, StoreError>>,
    {
        self.bounded_after(
            |store| store.watch_read_fence(user_id, read_after),
            local_read,
        )
        .await
    }

    #[cfg(feature = "hiqlite-store")]
    async fn bounded_after<T, R, F, Fut>(&self, min_applied_index: R, local_read: F) -> Option<T>
    where
        R: FnOnce(&HiqliteAuthStore) -> Option<u64>,
        F: FnOnce(Arc<HiqliteAuthStore>) -> Fut,
        Fut: std::future::Future<Output = Result<T, StoreError>>,
    {
        let bounded = self.bounded.as_ref()?;
        if !bounded.enabled {
            return None;
        }
        let min_applied_index = min_applied_index(&bounded.store)?;
        let store = Arc::clone(&bounded.store);
        #[cfg(feature = "cluster-read-cost-validation")]
        let revoke_after_local = Arc::clone(&bounded.revoke_after_next_local);
        let metrics = bounded.metrics.clone();
        #[cfg(feature = "cluster-read-cost-validation")]
        let post_query_metrics = metrics.clone();
        // A bounded read is an optimization, never a new application-visible
        // failure mode. Proof loss is represented by `None`; treat a local
        // SQL/row-mapping failure the same way and retry the existing Authority
        // path below. The Authority result remains the caller's result.
        metrics
            .run_bounded_replica_after(
                bounded.max_apply_lag_entries,
                min_applied_index,
                move || async move {
                    let result = local_read(store).await;
                    #[cfg(feature = "cluster-read-cost-validation")]
                    if revoke_after_local.swap(false, std::sync::atomic::Ordering::Relaxed) {
                        post_query_metrics.validation_revoke_bounded_proof();
                    }
                    result
                },
            )
            .await?
            .ok()
    }

    /// Per-user watch state for `item_ids`. Local only behind the
    /// read-your-write fence ([`Self::bounded_watch`]); Authority otherwise.
    pub async fn watch_map(
        &self,
        user_id: i64,
        item_ids: &[i64],
        read_after: Option<u64>,
    ) -> Result<Vec<(i64, WatchState)>, StoreError> {
        #[cfg(feature = "hiqlite-store")]
        {
            let ids = item_ids.to_vec();
            if let Some(result) = self
                .bounded_watch(user_id, read_after, move |store| async move {
                    store.local_watch_map(user_id, &ids).await
                })
                .await
            {
                return Ok(result);
            }
        }
        #[cfg(not(feature = "hiqlite-store"))]
        let _ = read_after;
        self.authority.watch_map(user_id, item_ids).await
    }

    /// One container's watched rollup, behind the same fence.
    pub async fn watch_rollup(
        &self,
        user_id: i64,
        item_id: i64,
        read_after: Option<u64>,
    ) -> Result<WatchRollup, StoreError> {
        #[cfg(feature = "hiqlite-store")]
        if let Some(result) = self
            .bounded_watch(user_id, read_after, move |store| async move {
                store.local_watch_rollup(user_id, item_id).await
            })
            .await
        {
            return Ok(result);
        }
        #[cfg(not(feature = "hiqlite-store"))]
        let _ = read_after;
        self.authority.watch_rollup(user_id, item_id).await
    }

    /// [`WatchStore::watch_summary`] behind the same fence.
    pub async fn watch_summary(
        &self,
        user_id: i64,
        item_ids: &[i64],
        container_ids: &[i64],
        read_after: Option<u64>,
    ) -> Result<WatchSummary, StoreError> {
        #[cfg(feature = "hiqlite-store")]
        {
            let items = item_ids.to_vec();
            let containers = container_ids.to_vec();
            if let Some(result) = self
                .bounded_watch(user_id, read_after, move |store| async move {
                    store
                        .local_watch_summary(user_id, &items, &containers)
                        .await
                })
                .await
            {
                return Ok(result);
            }
        }
        #[cfg(not(feature = "hiqlite-store"))]
        let _ = read_after;
        self.authority
            .watch_summary(user_id, item_ids, container_ids)
            .await
    }

    /// [`WatchStore::progress_rails`] behind the same fence.
    pub async fn progress_rails(
        &self,
        user_id: i64,
        limit: i64,
        read_after: Option<u64>,
    ) -> Result<ProgressRails, StoreError> {
        #[cfg(feature = "hiqlite-store")]
        if let Some(result) = self
            .bounded_watch(user_id, read_after, move |store| async move {
                store.local_progress_rails(user_id, limit).await
            })
            .await
        {
            return Ok(result);
        }
        #[cfg(not(feature = "hiqlite-store"))]
        let _ = read_after;
        self.authority.progress_rails(user_id, limit).await
    }

    pub async fn get_library(&self, id: i64) -> Result<Option<Library>, StoreError> {
        #[cfg(feature = "hiqlite-store")]
        if let Some(result) = self
            .bounded(move |store| async move { store.local_get_library(id).await })
            .await
        {
            return Ok(result);
        }
        self.authority.get_library(id).await
    }

    pub async fn list_libraries(&self) -> Result<Vec<Library>, StoreError> {
        #[cfg(feature = "hiqlite-store")]
        if let Some(result) = self
            .bounded(|store| async move { store.local_list_libraries().await })
            .await
        {
            return Ok(result);
        }
        self.authority.list_libraries().await
    }

    pub async fn get_item(&self, id: i64) -> Result<Option<Item>, StoreError> {
        #[cfg(feature = "hiqlite-store")]
        if let Some(result) = self
            .bounded(move |store| async move { store.local_get_item(id).await })
            .await
        {
            return Ok(result);
        }
        self.authority.get_item(id).await
    }

    pub async fn get_item_children(&self, parent_id: i64) -> Result<Vec<Item>, StoreError> {
        #[cfg(feature = "hiqlite-store")]
        if let Some(result) = self
            .bounded(move |store| async move { store.local_get_item_children(parent_id).await })
            .await
        {
            return Ok(result);
        }
        self.authority.get_item_children(parent_id).await
    }

    pub async fn list_top_items_in_genre(
        &self,
        library_id: i64,
        sort: ItemSort,
        offset: i64,
        limit: i64,
        genre: Option<&str>,
    ) -> Result<ItemPage, StoreError> {
        // Both the existing Authority implementation and this local variant
        // read the page and count in separate statements. The bounded permit
        // covers that whole operation but deliberately does not promise a
        // stronger cross-statement SQLite snapshot than Authority did.
        #[cfg(feature = "hiqlite-store")]
        {
            let genre = genre.map(str::to_owned);
            if let Some(result) = self
                .bounded(move |store| async move {
                    store
                        .local_list_top_items_in_genre(
                            library_id,
                            sort,
                            offset,
                            limit,
                            genre.as_deref(),
                        )
                        .await
                })
                .await
            {
                return Ok(result);
            }
        }
        self.authority
            .list_top_items_in_genre(library_id, sort, offset, limit, genre)
            .await
    }

    pub async fn home_preview_pages(
        &self,
        limit_per_library: i64,
    ) -> Result<Vec<HomePreviewPage>, StoreError> {
        #[cfg(feature = "hiqlite-store")]
        if let Some(result) = self
            .bounded(
                move |store| async move { store.local_home_preview_pages(limit_per_library).await },
            )
            .await
        {
            return Ok(result);
        }
        self.authority.home_preview_pages(limit_per_library).await
    }

    pub async fn recently_added(
        &self,
        library_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<RecentItem>, StoreError> {
        #[cfg(feature = "hiqlite-store")]
        if let Some(result) = self
            .bounded(
                move |store| async move { store.local_recently_added(library_id, limit).await },
            )
            .await
        {
            return Ok(result);
        }
        self.authority.recently_added(library_id, limit).await
    }

    /// Search is explicitly `NodeLocal`: Hiqlite's FTS tables are derived
    /// state and its Store implementation already uses `query_map`. Keeping
    /// the call on this named reader makes the classification visible to HTTP
    /// handlers without incorrectly applying the bounded-replica permit.
    pub async fn search_items(
        &self,
        query: &str,
        limit: i64,
    ) -> Result<Vec<RecentItem>, StoreError> {
        #[cfg(feature = "hiqlite-store")]
        if let Some(bounded) = &self.bounded {
            return bounded.store.search_items(query, limit).await;
        }
        self.authority.search_items(query, limit).await
    }

    pub async fn get_file(&self, id: i64) -> Result<Option<MediaFile>, StoreError> {
        #[cfg(feature = "hiqlite-store")]
        if let Some(result) = self
            .bounded(move |store| async move { store.local_get_file(id).await })
            .await
        {
            return Ok(result);
        }
        self.authority.get_file(id).await
    }

    pub async fn files_for_item(&self, item_id: i64) -> Result<Vec<MediaFile>, StoreError> {
        #[cfg(feature = "hiqlite-store")]
        if let Some(result) = self
            .bounded(move |store| async move { store.local_files_for_item(item_id).await })
            .await
        {
            return Ok(result);
        }
        self.authority.files_for_item(item_id).await
    }

    pub async fn child_counts(
        &self,
        ids: &[i64],
    ) -> Result<std::collections::HashMap<i64, i64>, StoreError> {
        #[cfg(feature = "hiqlite-store")]
        {
            let ids = ids.to_vec();
            if let Some(result) = self
                .bounded(move |store| async move { store.local_child_counts(&ids).await })
                .await
            {
                return Ok(result);
            }
        }
        self.authority.child_counts(ids).await
    }

    pub async fn item_max_heights(
        &self,
        ids: &[i64],
    ) -> Result<std::collections::HashMap<i64, i64>, StoreError> {
        #[cfg(feature = "hiqlite-store")]
        {
            let ids = ids.to_vec();
            if let Some(result) = self
                .bounded(move |store| async move { store.local_item_max_heights(&ids).await })
                .await
            {
                return Ok(result);
            }
        }
        self.authority.item_max_heights(ids).await
    }

    pub async fn item_media_facts(
        &self,
        ids: &[i64],
    ) -> Result<std::collections::HashMap<i64, MediaFacts>, StoreError> {
        #[cfg(feature = "hiqlite-store")]
        {
            let ids = ids.to_vec();
            if let Some(result) = self
                .bounded(move |store| async move { store.local_item_media_facts(&ids).await })
                .await
            {
                return Ok(result);
            }
        }
        self.authority.item_media_facts(ids).await
    }

    pub async fn media_shape(&self) -> Result<MediaShape, StoreError> {
        // Preserve the existing Authority method's multi-statement aggregate
        // semantics. The permit prevents an expired/stale replica result; it
        // is not a new transaction spanning these independent aggregates.
        #[cfg(feature = "hiqlite-store")]
        if let Some(result) = self
            .bounded(|store| async move { store.local_media_shape().await })
            .await
        {
            return Ok(result);
        }
        self.authority.media_shape().await
    }

    pub async fn get_file_probe_json(&self, file_id: i64) -> Result<Option<String>, StoreError> {
        #[cfg(feature = "hiqlite-store")]
        if let Some(result) = self
            .bounded(move |store| async move { store.local_get_file_probe_json(file_id).await })
            .await
        {
            return Ok(result);
        }
        self.authority.get_file_probe_json(file_id).await
    }
}

// Compile-time proof that the composed trait remains object-safe and that the
// production ownership shape can be constructed. If a future method makes
// `Store` non-object-safe, this function stops compiling before runtime tests.
#[allow(dead_code)]
fn assert_arc_dyn_store(store: std::sync::Arc<SqliteStore>) -> std::sync::Arc<dyn Store> {
    store
}

#[cfg(test)]
mod producer_recovery_schema_tests {
    use super::MEDIA_SESSION_PRODUCER_RECOVERY_SCHEMA;
    use crate::domain::MAX_DECODE_RESTRICTION_BYTES;

    /// The column's `CHECK` and the encoder's bound are one number.
    ///
    /// They are written in two places — a SQL literal and a Rust constant —
    /// and nothing but this stops them drifting. If they drift upward on the
    /// Rust side, every restriction between the two values encodes cleanly,
    /// is silently rejected by the constraint, and reports back to the caller
    /// as "the budget is already spent": a playback permanently loses its one
    /// automatic recovery with no error recorded anywhere.
    #[test]
    fn the_stored_restriction_cap_is_the_constant_the_encoder_enforces() {
        assert!(
            MEDIA_SESSION_PRODUCER_RECOVERY_SCHEMA
                .contains(&format!("<= {MAX_DECODE_RESTRICTION_BYTES}")),
            "the ledger's CHECK must name MAX_DECODE_RESTRICTION_BYTES exactly"
        );
    }
}

#[cfg(test)]
mod item_sort_order_tests {
    use super::item_sort_order_by;
    use crate::domain::ItemSort;

    /// Every library sort ends in a unique key.
    ///
    /// This is a text assertion on the clause, and it is deliberately not the
    /// fixture replay in `tests/store_contract.rs`. With movies alone that
    /// replay did not detect the tie-break's removal: SQLite returned the tied
    /// rows in rowid order, which is the order the fixture expects. It now
    /// does — the fixture's photo/movie pair ties on every visible key, and
    /// the grid query reads through `idx_items_library_kind`, which yields
    /// them in kind order — but that is still an observation of today's query
    /// plan. What SQLite does with tied rows is not promised by anything; it
    /// can change with the plan, the schema, an added index or the backend,
    /// and when it changes the symptom is a client paging by `offset` that
    /// reads two adjacent pages of two different orderings: an item on the
    /// seam shown twice and its neighbour never shown at all. A property
    /// nothing guarantees cannot be pinned by observing that it currently
    /// holds; it has to be pinned by requiring the clause that makes it true.
    ///
    /// `Added` ends in `id DESC` and the other four in `id ASC`. `Added` is
    /// not made symmetric on purpose: it already had a tie-break, and flipping
    /// it would reorder equal-`added_at` rows that viewers see today for
    /// nothing.
    #[test]
    fn every_sort_ends_in_a_unique_key() {
        for sort in [
            ItemSort::Title,
            ItemSort::Added,
            ItemSort::Year,
            ItemSort::Resolution,
            ItemSort::Recorded,
        ] {
            let clause = item_sort_order_by(sort);
            let last = clause
                .rsplit(',')
                .next()
                .expect("a non-empty ORDER BY")
                .split_whitespace()
                .collect::<Vec<_>>();
            assert_eq!(
                last.first().copied(),
                Some("id"),
                "{sort:?} ends in {clause:?}, which has no unique final key"
            );
            assert!(
                matches!(last.get(1).copied(), Some("ASC") | Some("DESC")),
                "{sort:?} ends in {clause:?}, whose final key has no explicit direction"
            );
        }
        assert!(item_sort_order_by(ItemSort::Added).ends_with("id DESC"));
    }

    /// The `resolution` sort ranks by height exactly the kinds whose DTO
    /// carries a `resolution`, and every other kind at -1.
    ///
    /// The clause and `ItemKind::carries_resolution` are two spellings of one
    /// rule — SQL cannot call the Rust predicate — so this reads the kind list
    /// out of the clause and compares it with the predicate for every kind. A
    /// kind added to one and not the other is a row the server ranks by a key
    /// the client never receives.
    #[test]
    fn resolution_ranks_exactly_the_kinds_that_carry_one() {
        use crate::domain::ItemKind;

        let clause = item_sort_order_by(ItemSort::Resolution);
        let list = clause
            .strip_prefix("CASE WHEN kind IN (")
            .and_then(|rest| rest.split_once(')'))
            .map(|(list, _)| list)
            .unwrap_or_else(|| panic!("{clause:?} does not rank by kind first"));
        let ranked: std::collections::BTreeSet<&str> = list
            .split(',')
            .map(|kind| kind.trim().trim_matches('\''))
            .collect();
        for kind in [
            ItemKind::Movie,
            ItemKind::Show,
            ItemKind::Season,
            ItemKind::Episode,
            ItemKind::Book,
            ItemKind::Audiobook,
            ItemKind::Folder,
            ItemKind::Video,
            ItemKind::Photo,
        ] {
            assert_eq!(
                ranked.contains(kind.as_str()),
                kind.carries_resolution(),
                "{kind:?}: the resolution sort and the DTO disagree about whether it has a resolution"
            );
        }
        assert!(clause.contains("ELSE -1 END DESC"), "{clause:?}");
    }

    /// The clause is one function, and every backend's page and count use it.
    ///
    /// Three copies of this `match` is how the SQLite and Hiqlite stores would
    /// drift apart, and a merge order that differs between backends is a
    /// defect no single-backend test can see — `for_each_backend` runs the
    /// Hiqlite voters only under `hiqlite-contract-tests`, so on an ordinary
    /// run the fixture replay never compares them at all.
    #[test]
    fn the_stores_do_not_carry_their_own_copies_of_the_order() {
        for source in [
            include_str!("sqlite/media.rs"),
            include_str!("hiqlite_media.rs"),
        ] {
            assert!(
                !source.contains("ItemSort::Title =>"),
                "a media store spells the library ORDER BY itself again"
            );
            assert!(source.contains("item_sort_order_by(sort)"));
        }
    }
}
