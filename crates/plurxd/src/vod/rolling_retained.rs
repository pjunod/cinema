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
    gate: Mutex<()>,
    refused: AtomicBool,
    published: AtomicBool,
}

impl RollingCollection {
    pub(crate) async fn capture(
        self: &Arc<Self>,
        source: PathBuf,
        name: &str,
        object: crate::rolling_output::CommittedObject,
    ) {
        if !retained_member(name, object.bytes) {
            self.refuse();
            return;
        }
        let Ok(_gate) = self.gate.try_lock() else {
            self.refuse();
            return;
        };
        if self.refused.load(Acquire) || Instant::now() >= self.deadline {
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
        let link = tokio::task::spawn_blocking(move || {
            let _owner = owner;
            let before = plurx_core::fs_secure::regular_file_identity_nofollow_blocking(&source)?;
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
        });
        if !matches!(
            tokio::time::timeout(Duration::from_secs(2), link).await,
            Ok(Ok(Ok(())))
        ) {
            self.refuse();
            return;
        }
        self.artifact
            .objects
            .lock()
            .expect("rolling inventory lock")
            .insert(name.to_owned(), object);
    }

    pub(crate) fn refuse(&self) {
        self.refused.store(true, Release);
    }

    /// Called only by the original actor's normal, duration-verified exit
    /// classification. The exact complete inventory includes mux/audio/tail.
    pub(crate) async fn finish(
        self: &Arc<Self>,
        inventory: crate::rolling_output::CompleteRollingInventory,
        playlist: &[u8],
        attempt: u64,
    ) -> Option<Arc<RollingArtifact>> {
        let _gate = self.gate.lock().await;
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
        self.artifact
            .complete
            .set(RollingComplete { rates, manifest })
            .ok()?;
        let shared = self.shared.upgrade()?;
        let mut state = shared
            .retained_artifacts
            .state
            .lock()
            .expect("retained registry lock");
        let cap = state.preparations.get(&self.artifact.nonce).copied()?;
        if cap != self.cap
            || state
                .rolling
                .contains_key(&self.artifact.production.binding())
        {
            return None;
        }
        state.bytes = state.bytes.checked_add(charge)?;
        state.preparations.remove(&self.artifact.nonce);
        state.rolling.insert(
            self.artifact.production.binding(),
            RollingEntry {
                artifact: Arc::clone(&self.artifact),
                idle_since: None,
            },
        );
        self.published.store(true, Release);
        Some(Arc::clone(&self.artifact))
    }
}

impl Drop for RollingCollection {
    fn drop(&mut self) {
        if self.published.load(Acquire) {
            return;
        }
        if let Some(shared) = self.shared.upgrade() {
            let mut state = shared
                .retained_artifacts
                .state
                .lock()
                .expect("retained registry lock");
            if state.preparations.contains_key(&self.artifact.nonce) {
                // Charge remains until the exact directory is actually gone.
                let Some(bytes) = state.bytes.checked_add(self.artifact.charge.load(Acquire))
                else {
                    return; // Keep the full reservation, never free overflowed credit.
                };
                state.bytes = bytes;
                state.preparations.remove(&self.artifact.nonce);
                state.rolling_retired.push_back(Arc::clone(&self.artifact));
            }
        }
    }
}

impl RollingArtifact {
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
        let artifact = self
            .state
            .lock()
            .expect("retained registry lock")
            .rolling_retired
            .front()
            .cloned();
        let Some(artifact) = artifact else {
            return;
        };
        if Arc::strong_count(&artifact) != 2 {
            return;
        }
        match remove_artifact_batch(&artifact.directory).await {
            Ok(true) => {
                let mut state = self.state.lock().expect("retained registry lock");
                if state
                    .rolling_retired
                    .front()
                    .is_some_and(|front| Arc::ptr_eq(front, &artifact))
                {
                    state.rolling_retired.pop_front();
                    state.bytes = state.bytes.saturating_sub(artifact.charge.load(Acquire));
                }
            }
            Ok(false) => {}
            Err(error) => tracing::warn!(target: "plurxd::vodserve", %error,
                "rolling retained cleanup remains charged"),
        }
    }
}

impl VodServe {
    pub(crate) async fn begin_rolling_collection(
        &self,
        production: Arc<crate::rolling_provenance::RollingProduction>,
        cap: u64,
        deadline: Instant,
    ) -> Option<Arc<RollingCollection>> {
        let shared = &self.shared;
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
        if cap == 0
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
            if !state.startup_done
                || !state.orphans.is_empty()
                || state.artifact_count() >= MAX_ARTIFACTS
                || state.bytes.checked_add(reserved)? >= cap
            {
                return None;
            }
            let nonce = uuid::Uuid::new_v4();
            let directory = state.namespace.as_ref()?.join(nonce.to_string());
            let available = cap.checked_sub(state.bytes.checked_add(reserved)?)?;
            state.preparations.insert(nonce, available);
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
        let collection = Arc::new(RollingCollection {
            shared: Arc::downgrade(shared),
            cap,
            deadline,
            gate: Mutex::new(()),
            refused: AtomicBool::new(false),
            published: AtomicBool::new(false),
            artifact: Arc::new(RollingArtifact {
                directory,
                production,
                nonce,
                charge: AtomicU64::new(0),
                objects: StdMutex::new(BTreeMap::new()),
                complete: OnceLock::new(),
            }),
        });
        let owner = Arc::clone(&collection);
        tokio::task::spawn_blocking(move || std::fs::create_dir(&owner.artifact.directory))
            .await
            .ok()?
            .ok()?;
        Some(collection)
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
        entry.idle_since = None;
        Some(Arc::clone(&entry.artifact))
    }
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

#[cfg(test)]
mod tests {
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
