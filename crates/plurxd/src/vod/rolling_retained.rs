//! Process-private rolling output in the existing retained namespace/budget.
//! No restart alias or received descriptor can create acquisition authority.
use super::*;
use std::collections::BTreeMap;
use std::sync::OnceLock;

pub(super) struct RollingEntry {
    pub(super) artifact: Arc<RollingArtifact>,
    pub(super) idle_since: Option<Instant>,
}

pub(crate) struct RollingArtifact {
    pub(crate) directory: PathBuf,
    pub(crate) production: Arc<crate::rolling_provenance::RollingProduction>,
    nonce: uuid::Uuid,
    charge: AtomicU64,
    objects: StdMutex<BTreeMap<String, crate::rolling_output::CommittedObject>>,
    complete: OnceLock<RollingComplete>,
    refused: AtomicBool,
    /// Abandoned before publication: never served, so deletable as soon as
    /// no capture holds the gate, even while its session still runs.
    abandoned: AtomicBool,
    /// The collection's capture gate, so the collector can tell when no
    /// optional link is in flight.
    gate: Arc<Mutex<()>>,
    /// What this is, for Activity: the operator sees a title, not a nonce.
    pub(crate) file_id: i64,
    pub(crate) item_title: String,
    /// Exactly what this artifact added to the registry's `bytes` when it was
    /// published or handed to the collector, written under the registry lock.
    /// The collector subtracts this, never the live `charge`: a capture racing
    /// an abandonment may raise `charge` after the abandonment read it.
    accounted: AtomicU64,
    /// Whether the collection that produced this artifact still exists. Its
    /// reference is not a reader, so Activity's "attached" and Stop discount
    /// it. Cleared when that collection drops.
    producer_alive: AtomicBool,
}

struct RollingComplete {
    rates: plurx_core::output_measurement::CompleteOutputRates,
    manifest: plurx_core::transcode::manifest::GenerationManifest,
}

/// Reserving the existing allowance precedes every optional hardlink. Drop
/// transfers actual bytes to the same collector; it never grants free credit
/// for names whose unlink has not completed. Blocking I/O retains this owner.
pub(crate) struct RollingCollection {
    shared: Weak<Shared>,
    artifact: Arc<RollingArtifact>,
    cap: u64,
    deadline: Instant,
    gate: Arc<Mutex<()>>,
    refused: AtomicBool,
    published: AtomicBool,
    /// Free-space headroom on the disk these links pin. Read, never sampled,
    /// on the capture path.
    headroom: Arc<crate::scratch_ledger::RetainedHeadroom>,
    /// Test seam: park the next capture after it took the gate and passed
    /// its checks, just before it links.
    #[cfg(test)]
    link_pause: StdMutex<
        Option<(
            std::sync::mpsc::SyncSender<()>,
            std::sync::mpsc::Receiver<()>,
        )>,
    >,
}

/// How old a headroom sample may be before a capture refuses: two sampler
/// ticks.
const HEADROOM_MAX_AGE: Duration = Duration::from_secs(60);

impl RollingCollection {
    #[cfg(test)]
    pub(crate) async fn hold_optional_capture_for_test(&self) -> tokio::sync::OwnedMutexGuard<()> {
        Arc::clone(&self.gate).lock_owned().await
    }
    pub(crate) fn capture(
        self: &Arc<Self>,
        source: PathBuf,
        name: &str,
        object: crate::rolling_output::CommittedObject,
    ) {
        if !retained_member(name, object.bytes) {
            self.refuse();
            return;
        }
        let Ok(gate) = Arc::clone(&self.gate).try_lock_owned() else {
            self.refuse();
            return;
        };
        if self.refused.load(Acquire) || Instant::now() >= self.deadline {
            self.refuse();
            return;
        }
        // The disk must still hold everything scratch is authorised to write
        // with this link's bytes pinned beside it. Atomics only: no statvfs
        // on the publication path.
        if let Err(reason) = self.headroom.admits(HEADROOM_MAX_AGE) {
            tracing::info!(
                target: "plurxd::vodserve",
                reason,
                "rolling retention abandoned: free-space headroom"
            );
            self.refuse();
            return;
        }
        let charge = {
            let objects = self
                .artifact
                .objects
                .lock()
                .expect("rolling inventory lock");
            // Keep room for the playlist in the bounded verified GET inventory.
            if objects.len() >= plurx_core::transcode::manifest::MAX_OBJECTS - 1
                || objects.contains_key(name)
            {
                self.refuse();
                return;
            }
            self.artifact.charge.load(Acquire).checked_add(object.bytes)
        };
        let Some(charge) = charge.filter(|charge| *charge <= self.cap) else {
            self.refuse();
            return;
        };
        self.artifact.charge.store(charge, Release);
        let destination = self.artifact.directory.join(name);
        let owner = Arc::clone(self);
        let expected_len = object.bytes;
        let name = name.to_owned();
        // One optional operation owns the gate and conservative charge. The
        // ordinary publisher never waits for filesystem proof collection.
        tokio::task::spawn_blocking(move || {
            let _gate = gate;
            #[cfg(test)]
            owner.pause_before_link_for_test();
            let result = (|| {
                let before =
                    plurx_core::fs_secure::regular_file_identity_nofollow_blocking(&source)?;
                if before.size != expected_len {
                    return Err(io::ErrorKind::InvalidData.into());
                }
                std::fs::hard_link(&source, &destination)?;
                let after =
                    plurx_core::fs_secure::regular_file_identity_nofollow_blocking(&destination)?;
                if !before.same_inode(after) || after.size != expected_len {
                    return Err(io::ErrorKind::InvalidData.into());
                }
                Ok::<_, io::Error>(())
            })();
            if result.is_err() || Instant::now() >= owner.deadline {
                owner.refuse();
                // A link that landed after an abandonment pins bytes nobody
                // will ever serve: take it back here, under the gate.
                if result.is_ok() {
                    let _ = std::fs::remove_file(&destination);
                }
                return;
            }
            if owner.artifact.abandoned.load(Acquire) {
                let _ = std::fs::remove_file(&destination);
                return;
            }
            owner
                .artifact
                .objects
                .lock()
                .expect("rolling inventory lock")
                .insert(name, object);
        });
    }

    /// Abandon this collection: idempotent, and a no-op once published —
    /// attached sessions serve straight from the artifact directory. The
    /// unpublished artifact goes to the collector at once, charged until its
    /// directory is actually gone, instead of when the session finally ends:
    /// its links pin disk the ledger no longer counts. Every refusal routes
    /// here: a refused capture, an unverified completion, a stopped session,
    /// and failed free-space headroom.
    pub(crate) fn refuse(&self) {
        self.abandon();
    }

    pub(crate) fn abandon(&self) {
        if self.published.load(Acquire) {
            return;
        }
        self.refused.store(true, Release);
        if self.artifact.abandoned.swap(true, AcqRel) {
            return;
        }
        let Some(shared) = self.shared.upgrade() else {
            return;
        };
        let mut state = shared
            .retained_artifacts
            .state
            .lock()
            .expect("retained registry lock");
        if self.published.load(Acquire) {
            return;
        }
        if state.preparations.contains_key(&self.artifact.nonce) {
            // The reservation becomes the charge for what was linked; the
            // collector frees exactly this amount once the directory is gone.
            let charge = self.artifact.charge.load(Acquire);
            let Some(bytes) = state.bytes.checked_add(charge) else {
                return;
            };
            state.bytes = bytes;
            self.artifact.accounted.store(charge, Release);
            state.preparations.remove(&self.artifact.nonce);
            state.rolling_retired.push_back(Arc::clone(&self.artifact));
        }
        state.collections.remove(&self.artifact.nonce);
    }

    /// Whether this collection was abandoned before publication.
    #[cfg(test)]
    pub(crate) fn abandoned(&self) -> bool {
        self.artifact.abandoned.load(Acquire)
    }

    /// Test seam: park the next capture once it holds the gate and has passed
    /// every check, just before its hard link. Returns (reached, resume).
    #[cfg(test)]
    pub(crate) fn pause_next_link_for_test(
        &self,
    ) -> (
        std::sync::mpsc::Receiver<()>,
        std::sync::mpsc::SyncSender<()>,
    ) {
        let (reached_tx, reached_rx) = std::sync::mpsc::sync_channel(1);
        let (resume_tx, resume_rx) = std::sync::mpsc::sync_channel(1);
        *self.link_pause.lock().expect("link pause lock") = Some((reached_tx, resume_rx));
        (reached_rx, resume_tx)
    }

    #[cfg(test)]
    fn pause_before_link_for_test(&self) {
        let pause = self.link_pause.lock().expect("link pause lock").take();
        if let Some((reached, resume)) = pause {
            let _ = reached.send(());
            let _ = resume.recv();
        }
    }

    #[cfg(test)]
    pub(crate) fn directory_for_test(&self) -> PathBuf {
        self.artifact.directory.clone()
    }

    #[cfg(test)]
    pub(crate) fn nonce_for_test(&self) -> uuid::Uuid {
        self.artifact.nonce
    }

    /// Abandon every unpublished collection on this node: the free-space
    /// headroom failed, or a scratch write hit ENOSPC. Driven by the disk,
    /// never by a ledger refusal, which these bytes are not part of.
    pub(crate) fn shed_all(&self) {
        if let Some(shared) = self.shared.upgrade() {
            shed_unpublished(&shared);
        }
    }

    /// Called only by the original actor's normal, duration-verified exit
    /// classification. The exact complete inventory includes mux/audio/tail.
    pub(crate) async fn finish(
        self: &Arc<Self>,
        inventory: crate::rolling_output::CompleteRollingInventory,
        playlist: &[u8],
        attempt: u64,
    ) -> Option<Arc<RollingArtifact>> {
        let _gate = tokio::time::timeout(Duration::from_secs(2), self.gate.lock())
            .await
            .ok()?;
        if self.refused.load(Acquire)
            || Instant::now() >= self.deadline
            || self.published.load(Acquire)
            || playlist.len() > 1 << 20
            || !self.artifact.production.current(attempt).await
        {
            self.refuse();
            return None;
        }
        let (rates, members, writing_nonce) = inventory;
        {
            let objects = self
                .artifact
                .objects
                .lock()
                .expect("rolling inventory lock");
            if members.len() != objects.len()
                || members.iter().any(|(name, object, _)| {
                    objects.get(name).is_none_or(|actual| {
                        actual.bytes != object.bytes || actual.digest != object.digest
                    })
                })
            {
                self.refuse();
                return None;
            }
        }
        let charge = self
            .artifact
            .charge
            .load(Acquire)
            .checked_add(playlist.len() as u64)?;
        if charge > self.cap {
            self.refuse();
            return None;
        }
        self.artifact.charge.store(charge, Release);
        let owner = Arc::clone(self);
        let playlist_body = playlist.to_vec();
        tokio::task::spawn_blocking(move || {
            let path = owner.artifact.directory.join("index.m3u8");
            std::fs::write(path, playlist_body)
        })
        .await
        .ok()?
        .ok()?;
        let mut objects = members
            .into_iter()
            .map(
                |(name, object, _)| plurx_core::transcode::manifest::GenerationObject {
                    name,
                    bytes: object.bytes,
                    sha256: hex::encode(object.digest),
                },
            )
            .collect::<Vec<_>>();
        objects.push(plurx_core::transcode::manifest::GenerationObject {
            name: "index.m3u8".to_owned(),
            bytes: playlist.len() as u64,
            sha256: hex::encode(Sha256::digest(playlist)),
        });
        // GenerationManifest's authenticated lookup is binary-search based;
        // media playback order remains in the separately verified playlist.
        objects.sort_by(|left, right| left.name.cmp(&right.name));
        // This in-memory inventory reuses only the authenticated GET snapshot
        // implementation. It is never stored/decoded as a cache or health proof.
        let manifest = plurx_core::transcode::manifest::GenerationManifest {
            format_version: 1,
            generation_id: format!("{}:{writing_nonce}", self.artifact.nonce),
            object_count: objects.len(),
            objects,
            manifest_digest: hex::encode(rates.identity),
            producer_health: None,
        };
        if !self.artifact.production.current(attempt).await
            || self.refused.load(Acquire)
            || Instant::now() >= self.deadline
        {
            self.refuse();
            return None;
        }
        let shared = self.shared.upgrade()?;
        let mut state = shared
            .retained_artifacts
            .state
            .lock()
            .expect("retained registry lock");
        // Checked under the registry lock that `abandon` also takes, so an
        // abandonment racing this publication either wins (nothing is
        // published) or loses (abandon becomes a no-op).
        if self.artifact.abandoned.load(Acquire) {
            return None;
        }
        self.artifact
            .complete
            .set(RollingComplete { rates, manifest })
            .ok()?;
        let cap = state.preparations.get(&self.artifact.nonce).copied()?;
        if cap != self.cap
            || state
                .rolling
                .contains_key(&self.artifact.production.binding())
        {
            return None;
        }
        state.bytes = state.bytes.checked_add(charge)?;
        self.artifact.accounted.store(charge, Release);
        state.preparations.remove(&self.artifact.nonce);
        state.rolling.insert(
            self.artifact.production.binding(),
            RollingEntry {
                artifact: Arc::clone(&self.artifact),
                idle_since: None,
            },
        );
        state.collections.remove(&self.artifact.nonce);
        self.published.store(true, Release);
        Some(Arc::clone(&self.artifact))
    }
}

impl Drop for RollingCollection {
    fn drop(&mut self) {
        // This collection's reference to the artifact is about to go; from
        // here on every other reference is a reader.
        self.artifact.producer_alive.store(false, Release);
        if self.published.load(Acquire) {
            return;
        }
        if let Some(shared) = self.shared.upgrade() {
            let mut state = shared
                .retained_artifacts
                .state
                .lock()
                .expect("retained registry lock");
            state.collections.remove(&self.artifact.nonce);
            if state.preparations.contains_key(&self.artifact.nonce) {
                // Charge remains until the exact directory is actually gone.
                let charge = self.artifact.charge.load(Acquire);
                let Some(bytes) = state.bytes.checked_add(charge) else {
                    return; // Keep the full reservation, never free overflowed credit.
                };
                state.bytes = bytes;
                self.artifact.accounted.store(charge, Release);
                state.preparations.remove(&self.artifact.nonce);
                state.rolling_retired.push_back(Arc::clone(&self.artifact));
            }
        }
    }
}

impl RollingArtifact {
    pub(crate) fn acquirable(&self) -> bool {
        !self.refused.load(Acquire)
    }

    /// Sessions reading this artifact, given one reference the caller knows
    /// is not a reader (the registry's entry). The producing collection's own
    /// reference is not a reader either: a freshly published artifact whose
    /// producer session is still alive has nobody reading it.
    fn readers(self: &Arc<Self>) -> usize {
        Arc::strong_count(self)
            .saturating_sub(1)
            .saturating_sub(usize::from(self.producer_alive.load(Acquire)))
    }

    /// Whether the collector may delete this retired directory now. A
    /// published artifact waits for its last reader (the registry's queue
    /// entry and the collector's own clone are the only two references). An
    /// abandoned one was never served, so it waits only for an in-flight
    /// capture: the gate is free, and any capture that takes it afterwards
    /// sees the abandonment and unlinks its own destination.
    fn deletable(self: &Arc<Self>) -> bool {
        if self.abandoned.load(Acquire) && self.complete.get().is_none() {
            return self.gate.try_lock().is_ok();
        }
        Arc::strong_count(self) == 2
    }
    pub(crate) async fn current_for_attachment(
        &self,
        production: &crate::rolling_provenance::RollingProduction,
    ) -> bool {
        attachment_facts_current(&self.refused, production.input_current()).await
    }
    pub(crate) fn object_etag(&self, name: &str) -> Option<String> {
        let object = self
            .complete
            .get()?
            .manifest
            .objects
            .iter()
            .find(|object| object.name == name)?;
        Some(format!(
            "\"{}\"",
            hex::encode(Sha256::digest(
                format!(
                    "{}\0{name}\0{}\0{}",
                    self.nonce, object.bytes, object.sha256
                )
                .as_bytes(),
            ))
        ))
    }
    pub(crate) fn bandwidth(&self) -> Option<plurx_core::transcode::OutputBandwidth> {
        let rates = &self.complete.get()?.rates;
        Some(plurx_core::transcode::OutputBandwidth {
            average_bps: rates.average_bps,
            peak_bps: rates.rfc_peak_bps,
        })
    }

    pub(crate) fn manifest(
        &self,
    ) -> Option<Arc<plurx_core::transcode::manifest::GenerationManifest>> {
        Some(Arc::new(self.complete.get()?.manifest.clone()))
    }
}

impl RetainedArtifactRegistry {
    pub(super) async fn collect_rolling(&self) {
        {
            let mut state = self.state.lock().expect("retained registry lock");
            for entry in state.rolling.values_mut() {
                if Arc::strong_count(&entry.artifact) == 1 {
                    entry.idle_since.get_or_insert_with(Instant::now);
                } else {
                    entry.idle_since = None;
                }
            }
            if state.rolling_retired.is_empty() {
                let key = state.rolling.iter().find_map(|(key, entry)| {
                    entry
                        .idle_since
                        .is_some_and(|since| since.elapsed() >= SESSION_IDLE_TTL)
                        .then_some(*key)
                });
                if let Some(key) = key {
                    let entry = state.rolling.remove(&key).expect("selected rolling entry");
                    state.rolling_retired.push_back(entry.artifact);
                }
            }
        }
        // The whole queue, not only its front: an artifact still held by a
        // live session must not block every abandoned one behind it.
        let deadline = Instant::now() + RETAINED_GC_TICK_BUDGET;
        let queued = self
            .state
            .lock()
            .expect("retained registry lock")
            .rolling_retired
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        for artifact in queued {
            if Instant::now() >= deadline {
                break;
            }
            if !artifact.deletable() {
                continue;
            }
            match remove_artifact_within(&artifact.directory, deadline).await {
                Ok(true) => {
                    let mut state = self.state.lock().expect("retained registry lock");
                    if let Some(position) = state
                        .rolling_retired
                        .iter()
                        .position(|queued| Arc::ptr_eq(queued, &artifact))
                    {
                        state.rolling_retired.remove(position);
                        // Exactly what publication or retirement added.
                        state.bytes = state.bytes.saturating_sub(artifact.accounted.load(Acquire));
                    }
                }
                Ok(false) => {}
                Err(error) => tracing::warn!(target: "plurxd::vodserve", %error,
                    "rolling retained cleanup remains charged"),
            }
        }
    }
}

/// Abandon every unpublished collection. Collected under the lock, abandoned
/// outside it, because `abandon` takes the same lock.
fn shed_unpublished(shared: &Shared) -> usize {
    let live = shared
        .retained_artifacts
        .state
        .lock()
        .expect("retained registry lock")
        .collections
        .values()
        .filter_map(Weak::upgrade)
        .collect::<Vec<_>>();
    let shed = live.len();
    for collection in live {
        collection.abandon();
    }
    shed
}

impl VodServe {
    /// What `budget` still admits in the retained registry.
    pub(crate) fn retained_remaining(&self, budget: u64) -> u64 {
        self.shared.retained_artifacts.remaining(budget)
    }

    #[allow(clippy::too_many_arguments)] // one collection's complete admission
    pub(crate) async fn begin_rolling_collection(
        &self,
        production: Arc<crate::rolling_provenance::RollingProduction>,
        budget: u64,
        estimate: u64,
        deadline: Instant,
        headroom: Arc<crate::scratch_ledger::RetainedHeadroom>,
        session_dir: &std::path::Path,
        file_id: i64,
        item_title: &str,
    ) -> Option<Arc<RollingCollection>> {
        let shared = &self.shared;
        // Hard links need one filesystem, and Windows has no device identity
        // to prove it with; there retention simply stays off.
        if cfg!(windows) {
            return None;
        }
        if headroom.admits(HEADROOM_MAX_AGE).is_err() {
            return None;
        }
        // A rolling-only manager has not necessarily entered VOD generation,
        // which normally creates this immediate owned base. Do not require a
        // synthetic VOD request, and never follow a substituted base symlink.
        match tokio::fs::symlink_metadata(&shared.base).await {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                tokio::fs::create_dir(&shared.base).await.ok()?;
            }
            _ => return None,
        }
        // No reservation for a collection that cannot take a single link: a
        // session directory on another filesystem (EXDEV) would leave the
        // feature silently inert while holding its allowance. Checked once
        // the base exists, so a fresh node's first collection is not refused.
        if !same_filesystem(session_dir, &shared.base).await {
            return None;
        }
        if estimate == 0
            || estimate > budget
            || deadline <= Instant::now()
            || !shared.retained_artifacts.own_namespace(&shared.base).await
        {
            return None;
        }
        shared.retained_artifacts.collect_orphans().await;
        let (nonce, directory) = {
            let mut state = shared
                .retained_artifacts
                .state
                .lock()
                .expect("retained registry lock");
            let reserved = state
                .preparations
                .values()
                .try_fold(0_u64, |sum, bytes| sum.checked_add(*bytes))?;
            // Reserve only this collection's bounded estimate; refuse when it
            // does not fit beside retained bytes and other reservations.
            if !state.startup_done
                || !state.orphans.is_empty()
                || state.artifact_count() >= MAX_ARTIFACTS
                || state.bytes.checked_add(reserved)?.checked_add(estimate)? > budget
            {
                return None;
            }
            let nonce = uuid::Uuid::new_v4();
            let directory = state.namespace.as_ref()?.join(nonce.to_string());
            state.preparations.insert(nonce, estimate);
            (nonce, directory)
        };
        let cap = shared
            .retained_artifacts
            .state
            .lock()
            .expect("retained registry lock")
            .preparations
            .get(&nonce)
            .copied()?;
        let gate = Arc::new(Mutex::new(()));
        let collection = Arc::new(RollingCollection {
            shared: Arc::downgrade(shared),
            cap,
            deadline,
            gate: Arc::clone(&gate),
            refused: AtomicBool::new(false),
            published: AtomicBool::new(false),
            headroom,
            #[cfg(test)]
            link_pause: StdMutex::new(None),
            artifact: Arc::new(RollingArtifact {
                directory,
                production,
                nonce,
                charge: AtomicU64::new(0),
                objects: StdMutex::new(BTreeMap::new()),
                complete: OnceLock::new(),
                refused: AtomicBool::new(false),
                abandoned: AtomicBool::new(false),
                gate,
                file_id,
                item_title: item_title.to_owned(),
                accounted: AtomicU64::new(0),
                producer_alive: AtomicBool::new(true),
            }),
        });
        let owner = Arc::clone(&collection);
        tokio::task::spawn_blocking(move || std::fs::create_dir(&owner.artifact.directory))
            .await
            .ok()?
            .ok()?;
        shared
            .retained_artifacts
            .state
            .lock()
            .expect("retained registry lock")
            .collections
            .insert(nonce, Arc::downgrade(&collection));
        Some(collection)
    }

    /// Abandon every unpublished rolling collection on this node. Called when
    /// the free-space headroom fails: shedding is driven by the disk, never
    /// by a scratch-ledger refusal, which these bytes were never part of.
    pub(crate) fn shed_rolling_collections(&self) -> usize {
        shed_unpublished(&self.shared)
    }

    /// Orphan directories and retired artifacts the collector still owes.
    pub(crate) fn retained_cleanup_pending(&self) -> usize {
        let state = self
            .shared
            .retained_artifacts
            .state
            .lock()
            .expect("retained registry lock");
        state.orphans.len() + state.rolling_retired.len()
    }

    /// The retained namespace's base, for the free-space sampler.
    pub(crate) fn retained_base(&self) -> PathBuf {
        self.shared.base.clone()
    }

    /// Every rolling artifact this node holds or is collecting, for Activity.
    /// Node-local: these registries are process-private.
    pub(crate) fn retained_snapshot(&self) -> Vec<RetainedOutputRow> {
        // Live collections are upgraded under the lock but dropped only after
        // it is released: an upgrade can be the last strong reference if its
        // session ends meanwhile, and `RollingCollection::drop` takes this
        // same (non-reentrant) lock.
        let (collecting, mut rows) = {
            let state = self
                .shared
                .retained_artifacts
                .state
                .lock()
                .expect("retained registry lock");
            let collecting = state
                .collections
                .values()
                .filter_map(Weak::upgrade)
                .collect::<Vec<_>>();
            let mut rows = Vec::new();
            for entry in state.rolling.values() {
                // The registry's own reference and the producing collection's
                // are not readers; any other reference is a session.
                let attached = entry.artifact.readers() > 0;
                rows.push(RetainedOutputRow::of(&entry.artifact, "retained", attached));
            }
            for artifact in &state.rolling_retired {
                rows.push(RetainedOutputRow::of(artifact, "releasing", false));
            }
            (collecting, rows)
        };
        for collection in &collecting {
            rows.push(RetainedOutputRow::of(
                &collection.artifact,
                "collecting",
                false,
            ));
        }
        drop(collecting);
        rows.sort_by(|left, right| left.nonce.cmp(&right.nonce));
        rows
    }

    /// Activity's Stop for one retained output: a collection is abandoned, a
    /// published artifact nobody reads is retired to the collector. Refused
    /// while a session is attached to it.
    pub(crate) fn release_retained_output(&self, nonce: uuid::Uuid) -> RetainedRelease {
        let collecting = {
            let mut state = self
                .shared
                .retained_artifacts
                .state
                .lock()
                .expect("retained registry lock");
            if state
                .rolling_retired
                .iter()
                .any(|artifact| artifact.nonce == nonce)
            {
                return RetainedRelease::Released;
            }
            // Attached means a session other than the producer reads it: the
            // producing collection (and the upload holding it) keeps a
            // reference until its session ends, and that is not a reader.
            let published = state
                .rolling
                .iter()
                .find(|(_, entry)| entry.artifact.nonce == nonce)
                .map(|(key, entry)| (*key, entry.artifact.readers() > 0));
            match published {
                Some((_, true)) => return RetainedRelease::Attached,
                Some((key, false)) => {
                    let entry = state.rolling.remove(&key).expect("selected rolling entry");
                    entry.artifact.refused.store(true, Release);
                    state.rolling_retired.push_back(entry.artifact);
                    return RetainedRelease::Released;
                }
                None => state.collections.get(&nonce).and_then(Weak::upgrade),
            }
        };
        match collecting {
            Some(collection) => {
                collection.abandon();
                RetainedRelease::Released
            }
            None => RetainedRelease::Unknown,
        }
    }

    pub(crate) fn acquire_rolling_output(
        &self,
        production: &crate::rolling_provenance::RollingProduction,
    ) -> Option<Arc<RollingArtifact>> {
        let mut state = self
            .shared
            .retained_artifacts
            .state
            .lock()
            .expect("retained registry lock");
        let entry = state.rolling.get_mut(&production.binding())?;
        if !entry.artifact.acquirable() {
            return None;
        }
        entry.idle_since = None;
        Some(Arc::clone(&entry.artifact))
    }

    /// Refuse this exact incarnation permanently without revoking issued
    /// body owners or releasing their accounting before collector unlink.
    pub(crate) fn refuse_rolling_output(&self, artifact: &Arc<RollingArtifact>) {
        artifact.refused.store(true, Release);
        let mut state = self
            .shared
            .retained_artifacts
            .state
            .lock()
            .expect("retained registry lock");
        let binding = artifact.production.binding();
        if state
            .rolling
            .get(&binding)
            .is_some_and(|entry| Arc::ptr_eq(&entry.artifact, artifact))
        {
            if let Some(entry) = state.rolling.remove(&binding) {
                state.rolling_retired.push_back(entry.artifact);
            }
        }
    }
}

/// One rolling artifact as Activity shows it.
#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub(crate) struct RetainedOutputRow {
    pub nonce: String,
    pub file_id: i64,
    pub title: String,
    /// `collecting` (a live session is linking its segments), `retained`
    /// (published, reusable) or `releasing` (queued for deletion).
    pub state: &'static str,
    pub bytes: u64,
    /// A session is reading it; Stop is refused until it ends.
    pub attached: bool,
}

impl RetainedOutputRow {
    fn of(artifact: &RollingArtifact, state: &'static str, attached: bool) -> Self {
        Self {
            nonce: artifact.nonce.to_string(),
            file_id: artifact.file_id,
            title: artifact.item_title.clone(),
            state,
            bytes: artifact.charge.load(Acquire),
            attached,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RetainedRelease {
    Released,
    Attached,
    Unknown,
}

/// Whether two paths are on one filesystem, the precondition for a hard link.
#[cfg(unix)]
async fn same_filesystem(left: &std::path::Path, right: &std::path::Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (
        tokio::fs::metadata(left).await,
        tokio::fs::metadata(right).await,
    ) {
        (Ok(left), Ok(right)) => left.dev() == right.dev(),
        _ => false,
    }
}

#[cfg(not(unix))]
async fn same_filesystem(_left: &std::path::Path, _right: &std::path::Path) -> bool {
    false
}

pub(super) fn owned_name(name: &str) -> bool {
    name == "init.mp4"
        || name == "index.m3u8"
        || name.strip_prefix("seg").is_some_and(|rest| {
            let digits = rest
                .strip_suffix(".m4s")
                .or_else(|| rest.strip_suffix(".ts"));
            digits.is_some_and(|digits| {
                !digits.is_empty()
                    && digits.len() <= 16
                    && digits.bytes().all(|b| b.is_ascii_digit())
            })
        })
}

fn retained_member(name: &str, bytes: u64) -> bool {
    name != "index.m3u8"
        && owned_name(name)
        && bytes > 0
        && bytes <= plurx_core::transcode::manifest::MAX_OBJECT_BYTES
}

async fn attachment_facts_current(
    refused: &AtomicBool,
    input_current: impl std::future::Future<Output = bool>,
) -> bool {
    // Source verification can yield. Sample irreversible refusal afterwards,
    // so a concurrent integrity transfer cannot be hidden by that await.
    input_current.await && !refused.load(Acquire)
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn rolling_attachment_refusal_is_observed_after_source_await() {
        use super::*;
        let refused = AtomicBool::new(false);
        assert!(attachment_facts_current(&refused, async { true }).await);
        assert!(!attachment_facts_current(&refused, async { false }).await);
        assert!(
            !attachment_facts_current(&refused, async {
                refused.store(true, Release);
                true
            })
            .await,
            "integrity refusal during source await remains visible"
        );
        assert!(
            !attachment_facts_current(&refused, async { true }).await,
            "refusal is one-way"
        );
    }

    #[test]
    fn rolling_member_bounds_refuse_unservable_and_foreign_proof_objects() {
        use super::retained_member;
        let cap = plurx_core::transcode::manifest::MAX_OBJECT_BYTES;
        assert!(retained_member("init.mp4", 1));
        assert!(retained_member("seg00000.ts", cap));
        assert!(retained_member("seg00000.m4s", 1));
        assert!(!retained_member("seg00000.ts", cap + 1));
        assert!(!retained_member("seg00000.ts", 0));
        assert!(!retained_member("../seg00000.ts", 1));
        assert!(!retained_member("index.m3u8", 1));
        assert!(!retained_member("seg00000.mp4", 1));
    }
}
