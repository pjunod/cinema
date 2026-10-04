//! Server-side coalescing for active playback progress before writes reach [`Store`].
//!
//! Players are deliberately free to report more often than durable consensus
//! should commit. The first beat is written immediately, intermediate beats
//! replace one pending value, and the newest pending value is flushed at the
//! ten-second boundary. A newly completed item bypasses the wait so its
//! watched transition and notification stay synchronous.
//!
//! The cluster growth harness compiles this file directly so its time-scaled
//! load exercises the production policy. Keep imports portable across that
//! source inclusion; a `crate::`-relative daemon import would break the gate.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use plurx_core::domain::WatchState;
use plurx_core::error::StoreError;
use plurx_core::store::{JellyfinProgressProvenance, JellyfinProgressWrite, Store};
use tokio::sync::Mutex;
use tokio::time::Instant;

const COMMIT_WINDOW: Duration = Duration::from_secs(10);
const MAX_FLUSH_ATTEMPTS: u32 = 8;
const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);
const ENTRY_SWEEP_AT: usize = 1024;
type ProgressKey = (i64, i64);
type SharedEntry = Arc<Mutex<Entry>>;
type TimeSource = Arc<dyn Fn() -> Instant + Send + Sync>;

#[derive(Clone)]
struct Pending {
    provenance: Option<JellyfinProgressProvenance>,
    position_ms: i64,
    duration_ms: Option<i64>,
}

#[derive(Default)]
struct Entry {
    last_commit: Option<Instant>,
    committed: Option<WatchState>,
    pending: Option<Pending>,
    worker_running: bool,
    retry_attempts: u32,
}

/// The result returned to a request handler. `watch` is always a coherent
/// durable row; reported position/duration are separate so side effects can
/// follow the current heartbeat without serializing a half-old optimistic
/// `WatchState` on the wire.
#[derive(Clone, Debug)]
pub struct ProgressUpdate {
    pub watch: WatchState,
    pub reported_position_ms: i64,
    pub reported_duration_ms: Option<i64>,
    pub committed: bool,
}

pub struct ProgressCoalescer {
    store: Arc<dyn Store>,
    entries: Mutex<HashMap<ProgressKey, SharedEntry>>,
    window: Duration,
    sweep_at: usize,
    now: TimeSource,
}

impl ProgressCoalescer {
    pub fn new(store: Arc<dyn Store>) -> Arc<Self> {
        Self::with_window(store, COMMIT_WINDOW)
    }

    fn with_window(store: Arc<dyn Store>, window: Duration) -> Arc<Self> {
        Self::with_limits(store, window, ENTRY_SWEEP_AT)
    }

    fn with_limits(store: Arc<dyn Store>, window: Duration, sweep_at: usize) -> Arc<Self> {
        Self::with_limits_and_time_source(store, window, sweep_at, Arc::new(Instant::now))
    }

    /// Build the production policy around a deterministic monotonic clock.
    ///
    /// The cluster growth harness uses this seam to preserve the real
    /// five-second-beat/ten-second-window ratio without turning a bounded CI
    /// gate into a ten-minute wall-clock campaign.
    #[doc(hidden)]
    #[allow(dead_code)] // Used when this source is included by plurx-cluster-check.
    pub(crate) fn with_time_source<F>(store: Arc<dyn Store>, now: F) -> Arc<Self>
    where
        F: Fn() -> Instant + Send + Sync + 'static,
    {
        Self::with_limits_and_time_source(store, COMMIT_WINDOW, ENTRY_SWEEP_AT, Arc::new(now))
    }

    fn with_limits_and_time_source(
        store: Arc<dyn Store>,
        window: Duration,
        sweep_at: usize,
        now: TimeSource,
    ) -> Arc<Self> {
        Arc::new(Self {
            store,
            entries: Mutex::new(HashMap::new()),
            window,
            sweep_at: sweep_at.max(1),
            now,
        })
    }

    async fn entry(&self, key: ProgressKey) -> Option<SharedEntry> {
        let mut entries = self.entries.lock().await;
        if let Some(entry) = entries.get(&key) {
            return Some(Arc::clone(entry));
        }
        if entries.len() >= self.sweep_at {
            let now = (self.now)();
            entries.retain(|_, entry| {
                // The map lock prevents a new caller from cloning this entry
                // while it is examined. Existing requests and flush workers
                // already own another Arc and therefore cannot be detached.
                if Arc::strong_count(entry) != 1 {
                    return true;
                }
                let Ok(slot) = entry.try_lock() else {
                    return true;
                };
                slot.worker_running
                    || slot.pending.is_some()
                    || slot
                        .last_commit
                        .is_some_and(|last| now.duration_since(last) < self.window)
            });
        }
        if entries.len() >= self.sweep_at {
            // The map is a hard cap, not merely a sweep trigger. A new stream
            // writes through until an idle slot becomes reclaimable.
            return None;
        }
        let entry = Arc::new(Mutex::new(Entry::default()));
        entries.insert(key, Arc::clone(&entry));
        Some(entry)
    }

    pub async fn put(
        self: &Arc<Self>,
        user_id: i64,
        item_id: i64,
        position_ms: i64,
        duration_ms: Option<i64>,
    ) -> Result<ProgressUpdate, StoreError> {
        self.put_inner(user_id, item_id, position_ms, duration_ms, None)
            .await?
            .ok_or_else(|| StoreError::Database("native progress unexpectedly refused".into()))
    }

    /// Retain the authenticated play and observation revision through the shared queue.
    pub async fn put_jellyfin(
        self: &Arc<Self>,
        write: JellyfinProgressWrite,
    ) -> Result<Option<ProgressUpdate>, StoreError> {
        if write.final_commit {
            return self.put_final(write).await;
        }
        self.put_inner(
            write.provenance.scope.user_id,
            write.item_id,
            write.position_ms,
            write.duration_ms,
            Some(write.provenance),
        )
        .await
    }

    /// Forced final commit for exactly one play. Another viewer's pending beat
    /// remains queued, with its original provenance, under the same entry lock.
    pub async fn put_final(
        self: &Arc<Self>,
        mut write: JellyfinProgressWrite,
    ) -> Result<Option<ProgressUpdate>, StoreError> {
        write.final_commit = true;
        let key = (write.provenance.scope.user_id, write.item_id);
        let entry = self.entry(key).await;
        let mut slot = match entry.as_ref() {
            Some(entry) => Some(entry.lock().await),
            None => None,
        };
        if let Some(slot) = slot.as_mut() {
            if slot
                .pending
                .as_ref()
                .and_then(|p| p.provenance.as_ref())
                .is_some_and(|p| {
                    p.play_id == write.provenance.play_id
                        && p.scope.token_digest == write.provenance.scope.token_digest
                        && p.scope.device_digest == write.provenance.scope.device_digest
                        && p.scope.client_family == write.provenance.scope.client_family
                        && p.scope.user_id == key.0
                })
            {
                slot.pending = None;
            }
        }
        let watch = self.store.put_jellyfin_progress(write, None).await?;
        if let Some(slot) = slot.as_mut() {
            slot.committed = match watch {
                Some(watch) => Some(watch),
                None => self.store.watch_state(key.0, key.1).await?,
            };
            slot.last_commit = slot.committed.as_ref().map(|_| (self.now)());
            slot.retry_attempts = 0;
        }
        Ok(watch.map(|watch| ProgressUpdate {
            reported_position_ms: watch.position_ms,
            reported_duration_ms: watch.duration_ms,
            watch,
            committed: true,
        }))
    }

    async fn commit(
        &self,
        key: ProgressKey,
        pending: &Pending,
        expected: Option<&WatchState>,
    ) -> Result<Option<WatchState>, StoreError> {
        if let Some(provenance) = pending.provenance.as_ref() {
            self.store
                .put_jellyfin_progress(
                    JellyfinProgressWrite {
                        provenance: provenance.clone(),
                        item_id: key.1,
                        position_ms: pending.position_ms,
                        duration_ms: pending.duration_ms,
                        final_commit: false,
                    },
                    expected,
                )
                .await
        } else if let Some(expected) = expected {
            self.store
                .put_progress_if_current(
                    key.0,
                    key.1,
                    expected,
                    pending.position_ms,
                    pending.duration_ms,
                )
                .await
        } else {
            self.store
                .put_progress(key.0, key.1, pending.position_ms, pending.duration_ms)
                .await
                .map(Some)
        }
    }

    async fn put_inner(
        self: &Arc<Self>,
        user_id: i64,
        item_id: i64,
        position_ms: i64,
        duration_ms: Option<i64>,
        provenance: Option<JellyfinProgressProvenance>,
    ) -> Result<Option<ProgressUpdate>, StoreError> {
        let observation = Pending {
            position_ms,
            duration_ms,
            provenance,
        };
        if let Some(provenance) = observation.provenance.as_ref() {
            let write = JellyfinProgressWrite {
                provenance: provenance.clone(),
                item_id,
                position_ms,
                duration_ms,
                final_commit: false,
            };
            if !self.store.jellyfin_progress_is_current(&write).await? {
                return Ok(None);
            }
        }
        let key = (user_id, item_id);
        let Some(entry) = self.entry(key).await else {
            let Some(watch) = self.commit(key, &observation, None).await? else {
                return Ok(None);
            };
            return Ok(Some(ProgressUpdate {
                reported_position_ms: watch.position_ms,
                reported_duration_ms: watch.duration_ms,
                watch,
                committed: true,
            }));
        };
        let mut slot = entry.lock().await;
        // Read the durable baseline while excluding this entry's flush worker.
        // Otherwise a flush can commit between the read and this lock, causing
        // a harmless but needless divergence reset and extra durable commit.
        let durable = self.store.watch_state(user_id, item_id).await?;
        let known_duration = self
            .store
            .files_for_item(item_id)
            .await?
            .into_iter()
            .filter_map(|file| file.duration_ms)
            .filter(|duration| *duration > 0)
            .max();
        if slot.committed != durable {
            // A non-coalescer writer moved the row. Any pending value was
            // observed before that write and is no longer safe to replay.
            slot.pending = None;
            slot.last_commit = None;
            slot.committed = durable;
            slot.retry_attempts = 0;
        }
        let now = (self.now)();
        let due = slot
            .last_commit
            .is_none_or(|last| now.duration_since(last) >= self.window);
        let position_ms = position_ms.max(0);
        let resolved_duration = slot
            .committed
            .as_ref()
            .and_then(|watch| watch.duration_ms)
            .or(known_duration)
            .or(duration_ms)
            .filter(|duration| *duration > 0);
        let position_ms = resolved_duration
            .map(|duration| position_ms.min(duration))
            .unwrap_or(position_ms);
        let newly_complete = slot.committed.as_ref().is_none_or(|watch| !watch.watched)
            && resolved_duration.is_some_and(|duration| {
                position_ms.saturating_mul(100) >= duration.saturating_mul(95)
            });

        if due || newly_complete {
            let Some(watch) = self.commit(key, &observation, None).await? else {
                return Ok(None);
            };
            slot.pending = None;
            slot.last_commit = Some((self.now)());
            slot.committed = Some(watch);
            return Ok(Some(ProgressUpdate {
                reported_position_ms: watch.position_ms,
                reported_duration_ms: watch.duration_ms,
                watch,
                committed: true,
            }));
        }

        slot.pending = Some(Pending {
            position_ms,
            duration_ms,
            provenance: observation.provenance,
        });
        slot.retry_attempts = 0;
        let watch = *slot.committed.as_ref().ok_or_else(|| {
            StoreError::Database(
                "progress coalescer has a clock without committed state".to_owned(),
            )
        })?;
        if !slot.worker_running {
            slot.worker_running = true;
            let coalescer = Arc::clone(self);
            let worker_entry = Arc::clone(&entry);
            tokio::spawn(async move {
                coalescer.flush_loop(key, worker_entry).await;
            });
        }
        Ok(Some(ProgressUpdate {
            watch,
            reported_position_ms: position_ms,
            reported_duration_ms: resolved_duration,
            committed: false,
        }))
    }

    async fn flush_loop(self: Arc<Self>, key: ProgressKey, entry: SharedEntry) {
        loop {
            let deadline = {
                let mut slot = entry.lock().await;
                if slot.pending.is_none() {
                    slot.worker_running = false;
                    return;
                }
                slot.last_commit
                    .map(|last| last + self.window)
                    .unwrap_or_else(|| (self.now)())
            };
            tokio::time::sleep_until(deadline).await;

            let mut slot = entry.lock().await;
            let Some(last) = slot.last_commit else {
                continue;
            };
            if (self.now)().duration_since(last) < self.window {
                continue;
            }
            let Some(pending) = slot.pending.take() else {
                continue;
            };
            match self
                .commit(
                    key,
                    &pending,
                    Some(
                        slot.committed
                            .as_ref()
                            .expect("pending requires committed state"),
                    ),
                )
                .await
            {
                Ok(Some(watch)) => {
                    slot.last_commit = Some((self.now)());
                    slot.committed = Some(watch);
                    slot.retry_attempts = 0;
                }
                Ok(None) => {
                    // Another durable writer changed this row while the beat
                    // waited. Adopt that state and discard the stale beat;
                    // overwriting a manual unwatch or an offline/Trakt merge
                    // would be worse than losing one intermediate heartbeat.
                    slot.committed = match self.store.watch_state(key.0, key.1).await {
                        Ok(current) => current,
                        Err(error) => {
                            slot.retry_attempts = slot.retry_attempts.saturating_add(1);
                            if slot.retry_attempts >= MAX_FLUSH_ATTEMPTS {
                                slot.worker_running = false;
                                tracing::error!(
                                    user_id = key.0,
                                    item_id = key.1,
                                    attempts = slot.retry_attempts,
                                    error = %error,
                                    "giving up refreshing a stale progress entry"
                                );
                                return;
                            }
                            slot.pending = Some(pending);
                            tracing::warn!(
                                user_id = key.0,
                                item_id = key.1,
                                error = %error,
                                "progress coalescer could not refresh after a competing write"
                            );
                            let delay = retry_delay(slot.retry_attempts);
                            drop(slot);
                            tokio::time::sleep(delay).await;
                            continue;
                        }
                    };
                    slot.last_commit = slot.committed.as_ref().map(|_| (self.now)());
                }
                Err(error) => {
                    slot.retry_attempts = slot.retry_attempts.saturating_add(1);
                    if slot.retry_attempts >= MAX_FLUSH_ATTEMPTS {
                        slot.worker_running = false;
                        tracing::error!(
                            user_id = key.0,
                            item_id = key.1,
                            attempts = slot.retry_attempts,
                            error = %error,
                            "giving up a coalesced progress write after bounded retries"
                        );
                        return;
                    }
                    slot.pending = Some(pending);
                    tracing::warn!(
                        user_id = key.0,
                        item_id = key.1,
                        error = %error,
                        "progress coalescer will retry the pending durable write"
                    );
                    let delay = retry_delay(slot.retry_attempts);
                    drop(slot);
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }

    /// Flush every accepted trailing beat immediately during graceful
    /// shutdown. Request draining stops new callers before this runs, and each
    /// entry mutex excludes its background worker while the final CAS is in
    /// flight.
    pub async fn drain(&self) -> Result<usize, StoreError> {
        let entries = self
            .entries
            .lock()
            .await
            .iter()
            .map(|(key, entry)| (*key, Arc::clone(entry)))
            .collect::<Vec<_>>();
        let mut flushed = 0;
        let mut first_error = None;
        for (key, entry) in entries {
            let mut slot = entry.lock().await;
            let Some(pending) = slot.pending.take() else {
                continue;
            };
            let Some(expected) = slot.committed.as_ref() else {
                slot.pending = Some(pending);
                if first_error.is_none() {
                    first_error = Some(StoreError::Database(
                        "progress coalescer has pending state without a durable baseline"
                            .to_owned(),
                    ));
                }
                continue;
            };
            match self.commit(key, &pending, Some(expected)).await {
                Ok(Some(watch)) => {
                    slot.committed = Some(watch);
                    slot.last_commit = Some((self.now)());
                    flushed += 1;
                }
                Ok(None) => match self.store.watch_state(key.0, key.1).await {
                    Ok(current) => {
                        slot.committed = current;
                        slot.last_commit = slot.committed.as_ref().map(|_| (self.now)());
                    }
                    Err(error) => {
                        slot.pending = Some(pending);
                        if first_error.is_none() {
                            first_error = Some(error);
                        }
                    }
                },
                Err(error) => {
                    slot.pending = Some(pending);
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(flushed),
        }
    }
}

fn retry_delay(attempt: u32) -> Duration {
    Duration::from_secs(
        1_u64
            .checked_shl(attempt.saturating_sub(1))
            .unwrap_or(u64::MAX),
    )
    .min(MAX_RETRY_DELAY)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use plurx_core::domain::{ItemKind, LibraryKind, NewItem, NewLibrary};
    use plurx_core::store::{LibraryStore, MediaStore, SqliteStore, UserStore};

    use super::*;

    async fn fixture() -> (Arc<dyn Store>, i64, i64) {
        let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let user = store
            .create_user("progress-coalescer", "hash", false)
            .await
            .expect("user");
        let library = store
            .create_library(&NewLibrary {
                name: "Progress".to_owned(),
                kind: LibraryKind::Movies,
                paths: vec![PathBuf::from("/progress")],
                anime: false,
            })
            .await
            .expect("library");
        let item = store
            .insert_item(&NewItem {
                library_id: library.id,
                kind: ItemKind::Movie,
                parent_id: None,
                title: "Progress proof".to_owned(),
                year: None,
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("item");
        (store, user.id, item)
    }

    async fn compatibility_play(
        store: &Arc<dyn Store>,
        user_id: i64,
        item_id: i64,
    ) -> JellyfinProgressWrite {
        use plurx_core::store::{
            JellyfinClientFamily, JellyfinEntityKind, JellyfinLoginWrite, JellyfinPlayActivation,
            JellyfinPlayScope, NewFileGrant, NewJellyfinPlay,
        };
        let hash = plurx_core::auth::hash_token;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_secs() as i64;
        let scope = JellyfinPlayScope {
            user_id,
            token_digest: hash("compatibility-token"),
            device_digest: hash("compatibility-device"),
            client_family: JellyfinClientFamily::AndroidTv,
        };
        store
            .replace_jellyfin_login(
                JellyfinLoginWrite {
                    token_hash: scope.token_digest.clone(),
                    user_id,
                    device_digest: scope.device_digest.clone(),
                    client_family: scope.client_family,
                    device_label: None,
                    expected_password_hash: "hash".into(),
                    created_at: now,
                },
                None,
            )
            .await
            .expect("login");
        store
            .set_jellyfin_compatibility(true)
            .await
            .expect("compatibility enabled");
        let switch_generation = store
            .jellyfin_compatibility_state()
            .await
            .expect("switch state")
            .generation
            .expect("switch generation");
        let file_id = store
            .upsert_file(
                item_id,
                "/progress/compatibility.mkv",
                100,
                1,
                &plurx_core::domain::ProbeResult {
                    duration_ms: Some(100_000),
                    ..Default::default()
                },
            )
            .await
            .expect("file");
        let item_wire = store
            .jellyfin_entity_ids(JellyfinEntityKind::Item, &[item_id])
            .await
            .expect("item identity")[0]
            .wire_id
            .clone();
        let file_wire = store
            .jellyfin_entity_ids(JellyfinEntityKind::File, &[file_id])
            .await
            .expect("file identity")[0]
            .wire_id
            .clone();
        // This module is also compiled by the cluster-load harness, which
        // deliberately has no UUID dependency. Unique valid wire IDs suffice
        // for this isolated storage fixture.
        static NEXT_PLAY: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let play_id = format!(
            "{:032x}",
            NEXT_PLAY.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let play = NewJellyfinPlay {
            play_id: play_id.clone(),
            scope: scope.clone(),
            playback_id: "compatibility-player".into(),
            item_id,
            file_id,
            item_wire_id: item_wire,
            file_wire_id: file_wire,
            source_fingerprint: hash("source"),
            profile_fingerprint: hash("profile"),
            native_request_fingerprint: hash("request"),
            selection_json: "{}".into(),
            source_origin_ms: 0,
            created_at_ms: now * 1000,
            media_grant_id: None,
            switch_generation,
        };
        assert!(store.create_jellyfin_play(play).await.expect("binding"));
        let grant = format!("progress-fixture-{play_id}");
        store
            .create_file_grant(NewFileGrant {
                id: grant.clone(),
                token_hash: hash(&grant),
                file_id,
                user_id,
                source_token_hash: scope.token_digest.clone(),
                created_at: now,
                expires_at: i64::MAX,
            })
            .await
            .expect("grant");
        assert!(store
            .activate_jellyfin_play(
                &play_id,
                &scope,
                JellyfinPlayActivation::DirectGrant(grant),
                now * 1000
            )
            .await
            .expect("activate"));
        let manual_revision = store
            .jellyfin_play(&play_id, &scope)
            .await
            .expect("play")
            .expect("play")
            .manual_revision;
        JellyfinProgressWrite {
            provenance: JellyfinProgressProvenance {
                play_id,
                scope,
                manual_revision,
            },
            item_id,
            position_ms: 1000,
            duration_ms: Some(100_000),
            final_commit: false,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn jellyfin_queue_keeps_pre_edit_revision_when_native_and_compatibility_beats_mix() {
        let (store, user_id, item_id) = fixture().await;
        let mut write = compatibility_play(&store, user_id, item_id).await;
        let coalescer = ProgressCoalescer::new(Arc::clone(&store));
        coalescer
            .put(user_id, item_id, 1000, Some(100_000))
            .await
            .expect("native leading");
        assert!(
            !coalescer
                .put_jellyfin(write.clone())
                .await
                .expect("compat queue")
                .expect("accepted")
                .committed
        );
        store
            .set_watched_tree_with_origin(user_id, item_id, true, Some(&write.provenance.scope))
            .await
            .expect("own mark");
        coalescer
            .put(user_id, item_id, 1000, Some(100_000))
            .await
            .expect("native authoritative after edit");
        write.provenance.manual_revision = 1;
        write.position_ms = 5000;
        assert!(
            !coalescer
                .put_jellyfin(write.clone())
                .await
                .expect("queued revision one")
                .expect("accepted")
                .committed
        );
        let before = store.watch_state(user_id, item_id).await.expect("before");
        store
            .set_watched_tree_with_origin(user_id, item_id, true, Some(&write.provenance.scope))
            .await
            .expect("explicit own no-op");
        assert_eq!(
            before,
            store
                .watch_state(user_id, item_id)
                .await
                .expect("same row and same clock")
        );
        assert_eq!(coalescer.drain().await.expect("drain fenced beat"), 0);
        assert_eq!(
            before,
            store
                .watch_state(user_id, item_id)
                .await
                .expect("preserved")
        );
        assert!(coalescer
            .put_jellyfin(write.clone())
            .await
            .expect("old revision admission")
            .is_none());
        write.provenance.manual_revision = 2;
        assert!(coalescer
            .put_jellyfin(write.clone())
            .await
            .expect("new observation")
            .is_some());
        coalescer
            .put(user_id, item_id, 8000, Some(100_000))
            .await
            .expect("native replaces pending provenance");
        store
            .set_watched_tree(user_id, item_id, true)
            .await
            .expect("external no-op fences compatibility only");
        assert_eq!(coalescer.drain().await.expect("native trailing"), 1);
        assert_eq!(
            store
                .watch_state(user_id, item_id)
                .await
                .expect("native row")
                .expect("native row")
                .position_ms,
            8000
        );
    }

    #[tokio::test(start_paused = true)]
    async fn jellyfin_final_discards_only_its_own_pending_and_preserves_native_viewer() {
        let (store, user_id, item_id) = fixture().await;
        let mut write = compatibility_play(&store, user_id, item_id).await;
        let coalescer = ProgressCoalescer::new(Arc::clone(&store));
        coalescer
            .put_jellyfin(write.clone())
            .await
            .expect("leading")
            .expect("leading");
        write.position_ms = 5000;
        coalescer
            .put_jellyfin(write.clone())
            .await
            .expect("pending")
            .expect("pending");
        coalescer
            .put(user_id, item_id, 8000, Some(100_000))
            .await
            .expect("another viewer");
        write.position_ms = 3000;
        let final_state = coalescer
            .put_final(write.clone())
            .await
            .expect("forced final")
            .expect("committed");
        assert!(final_state.committed);
        assert_eq!(final_state.watch.position_ms, 3000);
        assert_eq!(
            store
                .jellyfin_play(&write.provenance.play_id, &write.provenance.scope)
                .await
                .expect("play")
                .expect("play")
                .state,
            "ended"
        );
        assert!(coalescer
            .put_final(write.clone())
            .await
            .expect("duplicate")
            .is_none());
        assert_eq!(
            coalescer.drain().await.expect("other viewer still pending"),
            1
        );
        assert_eq!(
            store
                .watch_state(user_id, item_id)
                .await
                .expect("watch")
                .expect("watch")
                .position_ms,
            8000
        );
        assert!(coalescer
            .put_jellyfin(write)
            .await
            .expect("late beat")
            .is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn intermediate_beats_coalesce_and_the_latest_flushes() {
        let (store, user_id, item_id) = fixture().await;
        let coalescer = ProgressCoalescer::with_window(Arc::clone(&store), COMMIT_WINDOW);

        let first = coalescer
            .put(user_id, item_id, 1_000, Some(100_000))
            .await
            .expect("first");
        assert!(first.committed);

        tokio::time::advance(Duration::from_secs(5)).await;
        let middle = coalescer
            .put(user_id, item_id, 5_000, Some(100_000))
            .await
            .expect("middle");
        assert!(!middle.committed);
        assert_eq!(middle.watch.position_ms, 1_000);
        assert_eq!(middle.reported_position_ms, 5_000);
        assert_eq!(
            store
                .watch_state(user_id, item_id)
                .await
                .expect("watch")
                .expect("row")
                .position_ms,
            1_000
        );

        let latest = coalescer
            .put(user_id, item_id, 8_000, Some(100_000))
            .await
            .expect("latest");
        assert!(!latest.committed);
        tokio::time::advance(Duration::from_secs(5)).await;
        // The worker's flush crosses the blocking pool, so virtual time cannot
        // order that durable write before this read; under load the single
        // connection mutex is not FIFO and a one-shot read raced the UPDATE
        // (observed as 1_000 on a saturated 2-core host). Poll until the row
        // leaves the leading commit — bounded, so a coalescer that never
        // flushes still fails instead of hanging, and a flush that keeps the
        // wrong beat still fails the equality below.
        let mut position = 1_000;
        for _ in 0..1_000 {
            tokio::task::yield_now().await;
            position = store
                .watch_state(user_id, item_id)
                .await
                .expect("watch")
                .expect("row")
                .position_ms;
            if position != 1_000 {
                break;
            }
        }
        assert_eq!(
            position, 8_000,
            "the trailing flush keeps the newest coalesced beat"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_new_watched_transition_commits_synchronously() {
        let (store, user_id, item_id) = fixture().await;
        let coalescer = ProgressCoalescer::with_window(store, COMMIT_WINDOW);
        coalescer
            .put(user_id, item_id, 1_000, Some(100_000))
            .await
            .expect("first");
        tokio::time::advance(Duration::from_secs(2)).await;
        let finished = coalescer
            .put(user_id, item_id, 96_000, Some(100_000))
            .await
            .expect("finished");
        assert!(finished.committed);
        assert!(finished.watch.watched);
    }

    #[tokio::test(start_paused = true)]
    async fn a_trailing_beat_never_overwrites_a_competing_manual_write() {
        let (store, user_id, item_id) = fixture().await;
        let coalescer = ProgressCoalescer::with_window(Arc::clone(&store), COMMIT_WINDOW);
        coalescer
            .put(user_id, item_id, 1_000, Some(100_000))
            .await
            .expect("leading beat");
        coalescer
            .put(user_id, item_id, 5_000, Some(100_000))
            .await
            .expect("pending beat");
        store
            .set_watched(user_id, item_id, false)
            .await
            .expect("manual unwatch");
        let restarted = coalescer
            .put(user_id, item_id, 2_000, Some(100_000))
            .await
            .expect("restart beat");
        assert!(!restarted.watch.watched);
        assert_eq!(restarted.watch.position_ms, 2_000);

        tokio::time::advance(COMMIT_WINDOW).await;
        tokio::task::yield_now().await;
        let watch = store
            .watch_state(user_id, item_id)
            .await
            .expect("watch state")
            .expect("watch row");
        assert_eq!(watch.position_ms, 2_000);
        assert!(!watch.watched);
    }

    #[tokio::test(start_paused = true)]
    async fn a_repaired_probe_makes_the_watched_crossing_synchronous() {
        let (store, user_id, item_id) = fixture().await;
        let coalescer = ProgressCoalescer::with_window(Arc::clone(&store), COMMIT_WINDOW);
        coalescer
            .put(user_id, item_id, 1_000, None)
            .await
            .expect("leading beat without duration");
        store
            .upsert_file(
                item_id,
                "/progress/repaired.mkv",
                100,
                1,
                &plurx_core::domain::ProbeResult {
                    duration_ms: Some(100_000),
                    ..Default::default()
                },
            )
            .await
            .expect("repaired probe");

        let finished = coalescer
            .put(user_id, item_id, 96_000, None)
            .await
            .expect("crossing beat");
        assert!(finished.committed);
        assert!(finished.watch.watched);
    }

    #[tokio::test(start_paused = true)]
    async fn graceful_drain_commits_the_latest_pending_beat_immediately() {
        let (store, user_id, item_id) = fixture().await;
        let coalescer = ProgressCoalescer::with_window(Arc::clone(&store), COMMIT_WINDOW);
        coalescer
            .put(user_id, item_id, 1_000, Some(100_000))
            .await
            .expect("leading beat");
        coalescer
            .put(user_id, item_id, 7_000, Some(100_000))
            .await
            .expect("pending beat");

        assert_eq!(coalescer.drain().await.expect("drain"), 1);
        assert_eq!(
            store
                .watch_state(user_id, item_id)
                .await
                .expect("watch state")
                .expect("watch row")
                .position_ms,
            7_000
        );
    }

    #[tokio::test(start_paused = true)]
    async fn graceful_drain_continues_after_an_injected_entry_failure() {
        let (store, user_id, item_id) = fixture().await;
        let second_user = store
            .create_user("progress-drain-2", "hash", false)
            .await
            .expect("second user");
        let coalescer = ProgressCoalescer::with_window(Arc::clone(&store), COMMIT_WINDOW);

        coalescer
            .put(second_user.id, item_id, 1_000, Some(100_000))
            .await
            .expect("leading beat");
        coalescer
            .put(second_user.id, item_id, 7_000, Some(100_000))
            .await
            .expect("pending beat");

        let broken = Arc::new(Mutex::new(Entry {
            pending: Some(Pending {
                provenance: None,
                position_ms: 9_000,
                duration_ms: Some(100_000),
            }),
            ..Entry::default()
        }));
        coalescer
            .entries
            .lock()
            .await
            .insert((user_id, item_id), broken);

        assert!(coalescer.drain().await.is_err());
        assert_eq!(
            store
                .watch_state(second_user.id, item_id)
                .await
                .expect("watch state")
                .expect("watch row")
                .position_ms,
            7_000,
            "one broken entry must not abandon later pending flushes"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn idle_stream_entries_do_not_accumulate_without_bound() {
        let (store, user_id, item_id) = fixture().await;
        let second_user = store
            .create_user("progress-coalescer-2", "hash", false)
            .await
            .expect("second user");
        let coalescer = ProgressCoalescer::with_limits(Arc::clone(&store), COMMIT_WINDOW, 1);

        coalescer
            .put(user_id, item_id, 1_000, Some(100_000))
            .await
            .expect("first stream");
        tokio::time::advance(COMMIT_WINDOW).await;
        coalescer
            .put(second_user.id, item_id, 2_000, Some(100_000))
            .await
            .expect("second stream");

        let entries = coalescer.entries.lock().await;
        assert_eq!(entries.len(), 1);
        assert!(!entries.contains_key(&(user_id, item_id)));
    }

    #[tokio::test(start_paused = true)]
    async fn active_streams_beyond_the_cap_write_through_without_growing_the_map() {
        let (store, user_id, item_id) = fixture().await;
        let second_user = store
            .create_user("progress-cap-2", "hash", false)
            .await
            .expect("second user");
        let coalescer = ProgressCoalescer::with_limits(Arc::clone(&store), COMMIT_WINDOW, 1);

        coalescer
            .put(user_id, item_id, 1_000, Some(100_000))
            .await
            .expect("first stream");
        let overflow = coalescer
            .put(second_user.id, item_id, 2_000, Some(100_000))
            .await
            .expect("overflow stream");

        assert!(overflow.committed);
        assert_eq!(coalescer.entries.lock().await.len(), 1);
    }
}
