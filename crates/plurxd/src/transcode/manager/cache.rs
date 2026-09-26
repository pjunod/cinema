use super::*;

impl TranscodeManager {
    /// Prefer a currently verified shared generation, then preserve the
    /// established node-local lookup as the fallback. Replicated rows are not
    /// mount proof: the shared coordinator must still expose a live root.
    async fn cache_read_location(
        &self,
        recipe_hash: &str,
        cache: &CacheConfig,
    ) -> Result<Option<(CachedLocationIdentity, PathBuf)>, StoreError> {
        if let Some(shared) = self.shared_cache.as_ref() {
            if let (Some(storage_id), Some(root)) = (shared.storage_id(), shared.root().await) {
                if let Some(hit) = self.store.shared_cache_hit(recipe_hash, storage_id).await? {
                    // Placement and cross-node serving require the immutable
                    // object inventory. Legacy local entries remain readable,
                    // but never become shared solely because their directory
                    // happens to sit under the configured mount.
                    if hit.manifest_digest.is_some() {
                        return Ok(Some((
                            CachedLocationIdentity {
                                recipe_hash: recipe_hash.to_owned(),
                                node_id: hit.storage_id,
                                storage_class: "shared".to_owned(),
                                generation_id: Some(hit.generation_id),
                                relative_dir: hit.relative_dir,
                                manifest_digest: hit.manifest_digest,
                            },
                            root,
                        )));
                    }
                }
            }
        }
        self.local_cache_read_location(recipe_hash, cache).await
    }

    async fn local_cache_read_location(
        &self,
        recipe_hash: &str,
        cache: &CacheConfig,
    ) -> Result<Option<(CachedLocationIdentity, PathBuf)>, StoreError> {
        Ok(self
            .store
            .cache_hit(recipe_hash, &cache.node_id)
            .await?
            .map(|hit| {
                (
                    CachedLocationIdentity {
                        recipe_hash: recipe_hash.to_owned(),
                        node_id: cache.node_id.clone(),
                        storage_class: hit.storage_class,
                        generation_id: None,
                        relative_dir: hit.relative_dir,
                        manifest_digest: hit.manifest_digest,
                    },
                    cache.dir.clone(),
                )
            }))
    }

    pub(super) async fn invalidate_cache_location(
        &self,
        location: &CachedLocationIdentity,
        reason: &'static str,
    ) -> bool {
        if location.storage_class == "shared" {
            if let Some(shared) = self.shared_cache.as_ref() {
                shared.report_io_failure(reason).await;
            }
            // A read failure is evidence about this node's admitted mount,
            // not proof that every voter lost the portable generation. Keep
            // the replicated pointer and its reader pins intact so healthy
            // members may continue serving it; fenced GC owns global
            // retirement.
            tracing::warn!(
                target: "plurxd::transcode",
                recipe = %location.recipe_hash,
                storage = %location.node_id,
                reason,
                "shared cache read failed; disabled this member without retiring the generation"
            );
            return false;
        }
        Self::invalidate_cache_location_with_store(self.store.as_ref(), location, reason).await
    }

    async fn invalidate_cache_location_with_store(
        store: &dyn Store,
        location: &CachedLocationIdentity,
        reason: &'static str,
    ) -> bool {
        match store
            .invalidate_cache_entry(
                &location.recipe_hash,
                &location.node_id,
                &location.storage_class,
                &location.relative_dir,
                location.manifest_digest.as_deref(),
            )
            .await
        {
            Ok(invalidated) => {
                tracing::warn!(
                    target: "plurxd::transcode",
                    recipe = %location.recipe_hash,
                    node = %location.node_id,
                    storage_class = %location.storage_class,
                    relative_dir = %location.relative_dir,
                    invalidated,
                    reason,
                    "cache location failed integrity validation"
                );
                invalidated
            }
            Err(error) => {
                tracing::error!(
                    target: "plurxd::transcode",
                    recipe = %location.recipe_hash,
                    node = %location.node_id,
                    storage_class = %location.storage_class,
                    relative_dir = %location.relative_dir,
                    %error,
                    reason,
                    "could not invalidate a corrupt cache location"
                );
                false
            }
        }
    }

    pub(super) fn fail_cached_session_integrity(
        self: &Arc<Self>,
        session_id: &str,
        session: &Arc<Session>,
        reason: &'static str,
    ) {
        // Publish the response-visible verdict before any database, mount,
        // task-scheduling, or retirement operation. The exact response owner
        // can therefore authenticate this failure even after cleanup removes
        // the Session from the process-local registry.
        session.fail(PlaylistError::SessionFailed(
            CACHED_MEDIA_INTEGRITY_FAILURE.to_owned(),
        ));
        if session
            .cache_integrity_cleanup_started
            .compare_exchange(false, true, AcqRel, Acquire)
            .is_err()
        {
            return;
        }

        let manager = Arc::clone(self);
        let session = Arc::clone(session);
        let session_id = session_id.to_owned();
        let (_commit, retirement) = spawn_rolling_retirement_owner(
            Arc::clone(&self.sessions),
            Arc::clone(&self.retired_presentations),
            Arc::clone(&self.active_session_count),
            Arc::clone(&self.store),
            Arc::clone(&self.recent_marker_ambiguities),
            session_id.clone(),
            Arc::clone(&session),
            None,
            "failed",
        );
        tokio::spawn(async move {
            // This is the sole cleanup owner. It is detached before the
            // detecting request returns, so an outer playlist deadline or a
            // disconnected segment request cannot cancel invalidation between
            // the synchronous failure store and exact retirement.
            if let Some(location) = &session.cache_location {
                manager.invalidate_cache_location(location, reason).await;
            }
            let _ = retirement.wait().await;
        });
    }

    /// Prove that the complete local generation for this exact recipe is
    /// byte-verified. This is the non-reserving subset of `serve_cached`: it
    /// neither creates a session nor updates last-used metadata.
    ///
    /// It takes the plan and nothing else: the plan carries which source it is
    /// for, so there is no second argument that could name a different film.
    /// What it does *not* yet prove is that the bytes on disk are still the
    /// bytes that were measured — `plan.source_binding()` records whether the
    /// facts were descriptor-bound.
    ///
    /// A health receipt does not belong in this answer, and the temptation to
    /// put one here is worth naming. Its only caller feeds cluster offer
    /// eligibility, so answering `false` does not decline to *keep* a
    /// generation — it moves a viewer to a node that has to encode the title
    /// again. Unqualified bytes may still be served to the viewer waiting for
    /// them; refusing to reuse them is a decision for the paths that publish
    /// and claim, not for the one that says which node already has the film.
    pub(super) async fn verified_cache_hit(&self, plan: &ResolvedTranscode) -> bool {
        let Some(cache) = self.cache.as_ref() else {
            return false;
        };
        let Some(digest) = self.digest() else {
            return false;
        };
        let hash = self.effective_recipe(&digest, plan, false).hash();
        let Some((identity, cache_root)) =
            self.cache_read_location(&hash, cache).await.ok().flatten()
        else {
            return false;
        };
        // Legacy completions do not carry a byte inventory and therefore
        // cannot make the stronger cluster placement claim.
        let Some(expected_manifest) = identity.manifest_digest.clone() else {
            return false;
        };
        let now = Instant::now();
        {
            let mut verdicts = self
                .cache_offer_verdicts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            verdicts.retain(|_, verdict| match verdict {
                CacheOfferVerdict::Pending { started_at, .. } => {
                    now.saturating_duration_since(*started_at) <= CACHE_OFFER_VERDICT_TTL
                }
                CacheOfferVerdict::Ready { observed_at, .. } => {
                    now.saturating_duration_since(*observed_at) <= CACHE_OFFER_VERDICT_TTL
                }
            });
            match verdicts.get(&hash) {
                Some(CacheOfferVerdict::Ready {
                    identity: cached,
                    verified,
                    ..
                }) if cached == &identity => return *verified,
                Some(CacheOfferVerdict::Pending {
                    identity: cached, ..
                }) if cached == &identity => return false,
                _ => {
                    verdicts.remove(&hash);
                }
            }
            if verdicts.len() >= MAX_CACHE_OFFER_VERDICTS {
                return false;
            }
        }
        let Ok(permit) = Arc::clone(&self.cache_offer_verifier).try_acquire_owned() else {
            return false;
        };
        {
            let mut verdicts = self
                .cache_offer_verdicts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            // Another request may have published while this one acquired the
            // sole verifier. Never replace a fresh verdict with pending.
            if verdicts.contains_key(&hash) {
                return false;
            }
            verdicts.insert(
                hash.clone(),
                CacheOfferVerdict::Pending {
                    identity: identity.clone(),
                    started_at: now,
                },
            );
        }
        let verdicts = Arc::clone(&self.cache_offer_verdicts);
        let store = Arc::clone(&self.store);
        let readers = self.cache_readers.clone();
        let shared_cache = self.shared_cache.clone();
        tokio::spawn(async move {
            let verification = Self::verify_cache_offer_location(
                Arc::clone(&store),
                readers,
                shared_cache.clone(),
                cache_root,
                identity.clone(),
                expected_manifest,
            )
            .await;
            if verification.revoke_shared_member && identity.storage_class == "shared" {
                if let Some(shared_cache) = shared_cache.as_ref() {
                    shared_cache
                        .report_io_failure("offer_integrity_failed")
                        .await;
                }
            }
            let mut verdicts = verdicts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if matches!(
                verdicts.get(&hash),
                Some(CacheOfferVerdict::Pending { identity: pending, .. }) if pending == &identity
            ) {
                verdicts.insert(
                    hash,
                    CacheOfferVerdict::Ready {
                        identity,
                        verified: verification.verified,
                        observed_at: Instant::now(),
                    },
                );
            }
            drop(permit);
        });
        false
    }

    async fn verify_cache_offer_location(
        store: Arc<dyn Store>,
        readers: crate::cachekeep::ActiveCacheReaders,
        shared_cache: Option<Arc<crate::shared_cache::SharedCacheCoordinator>>,
        cache_root: PathBuf,
        identity: CachedLocationIdentity,
        expected_manifest: String,
    ) -> CacheOfferVerification {
        if identity.storage_class == "shared" {
            let Some(coordinator) = shared_cache else {
                return CacheOfferVerification {
                    verified: false,
                    revoke_shared_member: false,
                };
            };
            let task_store = Arc::clone(&store);
            return coordinator
                .run_mount_io("offer_verification_timeout", async move {
                    Ok(Self::verify_cache_offer_location_inner(
                        task_store,
                        readers,
                        cache_root,
                        identity,
                        expected_manifest,
                    )
                    .await)
                })
                .await
                .unwrap_or(CacheOfferVerification {
                    verified: false,
                    revoke_shared_member: false,
                });
        }
        Self::verify_cache_offer_location_inner(
            store,
            readers,
            cache_root,
            identity,
            expected_manifest,
        )
        .await
    }

    async fn verify_cache_offer_location_inner(
        store: Arc<dyn Store>,
        readers: crate::cachekeep::ActiveCacheReaders,
        cache_root: PathBuf,
        identity: CachedLocationIdentity,
        expected_manifest: String,
    ) -> CacheOfferVerification {
        let Some(_lookup) = readers.begin_lookup(&identity.recipe_hash) else {
            return CacheOfferVerification {
                verified: false,
                revoke_shared_member: false,
            };
        };
        let shared_pin = if identity.storage_class == "shared" {
            let Some(generation_id) = identity.generation_id.as_ref() else {
                return CacheOfferVerification {
                    verified: false,
                    revoke_shared_member: false,
                };
            };
            let now_ms = unix_ms();
            let pin = CacheConsumerPin {
                storage_id: identity.node_id.clone(),
                recipe_hash: identity.recipe_hash.clone(),
                generation_id: generation_id.clone(),
                consumer_kind: CacheConsumerKind::MediaSession,
                consumer_id: format!("offer-{}", uuid::Uuid::new_v4().simple()),
                consumer_epoch: 1,
                expires_at_ms: now_ms.saturating_add(SHARED_LOOKUP_PIN_MS),
            };
            match store.acquire_cache_consumer_pin(&pin, now_ms).await {
                Ok(true) => Some(pin),
                Ok(false) | Err(_) => {
                    return CacheOfferVerification {
                        verified: false,
                        revoke_shared_member: false,
                    };
                }
            }
        } else {
            None
        };

        let verified = async {
            let Some(dir) =
                crate::cachekeep::validated_entry_dir(&cache_root, &identity.relative_dir).await
            else {
                if identity.storage_class != "shared" {
                    let _ = Self::invalidate_cache_location_with_store(
                        store.as_ref(),
                        &identity,
                        "unsafe_relative_path",
                    )
                    .await;
                }
                return false;
            };
            let manifest = match crate::manifest_cache::load(
                crate::manifest_cache::GenerationKey {
                    cache_root,
                    node_id: identity.node_id.clone(),
                    recipe_hash: identity.recipe_hash.clone(),
                    storage_class: identity.storage_class.clone(),
                    relative_dir: identity.relative_dir.clone(),
                    manifest_digest: expected_manifest,
                },
                &dir,
            )
            .await
            {
                Ok(manifest) => manifest,
                Err(_) => {
                    if identity.storage_class != "shared" {
                        let _ = Self::invalidate_cache_location_with_store(
                            store.as_ref(),
                            &identity,
                            "manifest_invalid",
                        )
                        .await;
                    }
                    return false;
                }
            };
            let playlist_valid = manifest
                .read_verified_playlist(&dir, "index.m3u8")
                .await
                .ok()
                .flatten()
                .as_deref()
                .and_then(|bytes| std::str::from_utf8(bytes).ok())
                .and_then(validated_vod_part)
                .is_some();
            if !playlist_valid && identity.storage_class != "shared" {
                let _ = Self::invalidate_cache_location_with_store(
                    store.as_ref(),
                    &identity,
                    "playlist_invalid_vod",
                )
                .await;
            }
            playlist_valid
        }
        .await;

        if let Some(pin) = shared_pin.as_ref() {
            // Best effort: the durable pin expires if verification cleanup
            // cannot release it immediately.
            crate::store_result::observe(
                crate::store_result::Operation::ReleaseSharedLookupPinAfterVerification,
                crate::store_result::Discard::BestEffort,
                store
                    .release_cache_consumer_pin(
                        &pin.storage_id,
                        &pin.recipe_hash,
                        &pin.generation_id,
                        pin.consumer_kind,
                        &pin.consumer_id,
                        pin.consumer_epoch,
                    )
                    .await,
            );
        };
        CacheOfferVerification {
            verified,
            revoke_shared_member: !verified && shared_pin.is_some(),
        }
    }

    async fn prepare_shared_cached_read(
        store: Arc<dyn Store>,
        coordinator: Arc<crate::shared_cache::SharedCacheCoordinator>,
        cache_root: PathBuf,
        identity: CachedLocationIdentity,
        expected_manifest: String,
        consumer_id: String,
    ) -> Result<PreparedSharedCacheRead, String> {
        coordinator
            .run_mount_io("shared_serve_read_timeout", async move {
                let generation_id = identity
                    .generation_id
                    .as_ref()
                    .ok_or_else(|| "shared cache generation identity is missing".to_owned())?;
                let now_ms = unix_ms();
                let pin = CacheConsumerPin {
                    storage_id: identity.node_id.clone(),
                    recipe_hash: identity.recipe_hash.clone(),
                    generation_id: generation_id.clone(),
                    consumer_kind: CacheConsumerKind::MediaSession,
                    consumer_id,
                    consumer_epoch: 1,
                    expires_at_ms: now_ms.saturating_add(SHARED_LOOKUP_PIN_MS),
                };
                if !store
                    .acquire_cache_consumer_pin(&pin, now_ms)
                    .await
                    .map_err(|error| error.to_string())?
                {
                    return Err(
                        "shared cache generation retired before it could be pinned".to_owned()
                    );
                }
                // This random session-id pin is only the lookup-to-activation
                // bridge. The activated route acquires its durable
                // incarnation/epoch pin separately. Always retire the bridge
                // at its hard lifetime even when activation succeeds or its
                // caller is cancelled; otherwise a permanently hot generation
                // accumulates one expired durable row per playback forever.
                let cleanup_store = Arc::clone(&store);
                let cleanup_pin = pin.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(
                        u64::try_from(SHARED_LOOKUP_PIN_MS).unwrap_or(u64::MAX),
                    ))
                    .await;
                    // Best effort: this delayed release only shortens the
                    // durable pin's own bounded expiry.
                    crate::store_result::observe(
                        crate::store_result::Operation::ReleaseExpiredSharedLookupPin,
                        crate::store_result::Discard::BestEffort,
                        cleanup_store.release_cache_consumer_pin(
                            &cleanup_pin.storage_id,
                            &cleanup_pin.recipe_hash,
                            &cleanup_pin.generation_id,
                            cleanup_pin.consumer_kind,
                            &cleanup_pin.consumer_id,
                            cleanup_pin.consumer_epoch,
                        )
                        .await,
                    );
                });
                let prepared = async {
                    let dir =
                        crate::cachekeep::validated_entry_dir(&cache_root, &identity.relative_dir)
                            .await
                            .ok_or_else(|| "shared cache generation path is unsafe".to_owned())?;
                    tokio::fs::metadata(dir.join("index.m3u8"))
                        .await
                        .map_err(|error| {
                            format!("shared cache playlist is unavailable: {error}")
                        })?;
                    let manifest = crate::manifest_cache::load(
                        crate::manifest_cache::GenerationKey {
                            cache_root,
                            node_id: identity.node_id.clone(),
                            recipe_hash: identity.recipe_hash.clone(),
                            storage_class: identity.storage_class.clone(),
                            relative_dir: identity.relative_dir.clone(),
                            manifest_digest: expected_manifest,
                        },
                        &dir,
                    )
                    .await?;
                    let playlist_valid = manifest
                        .read_verified_playlist(&dir, "index.m3u8")
                        .await
                        .ok()
                        .flatten()
                        .as_deref()
                        .and_then(|bytes| std::str::from_utf8(bytes).ok())
                        .and_then(validated_vod_part)
                        .is_some();
                    if !playlist_valid {
                        return Err("shared cache playlist failed integrity validation".to_owned());
                    }
                    Ok(PreparedSharedCacheRead { dir, manifest })
                }
                .await;
                if prepared.is_err() {
                    // Best effort: failed preparation cannot consume the
                    // bytes, and the durable pin expires without this release.
                    crate::store_result::observe(
                        crate::store_result::Operation::ReleaseSharedLookupPinAfterPreparationFailure,
                        crate::store_result::Discard::BestEffort,
                        store.release_cache_consumer_pin(
                            &pin.storage_id,
                            &pin.recipe_hash,
                            &pin.generation_id,
                            pin.consumer_kind,
                            &pin.consumer_id,
                            pin.consumer_epoch,
                        )
                        .await,
                    );
                }
                prepared
            })
            .await
    }

    /// Serve a finished transcode, if this exact one has already been made.
    ///
    /// Registers a session with no child process. That is the whole shape of a
    /// hit: there is nothing to run, nothing to watch, nothing to pace, and
    /// nothing to suspend — only a directory of segments that already exist
    /// and a viewer to point at them. It is still a session because the
    /// activity page should show somebody watching, and because the idle
    /// reaper is what eventually forgets them.
    pub(super) async fn serve_cached(
        &self,
        file: &plurx_core::domain::MediaFile,
        opts: &TranscodeOptions,
        plan: &ResolvedTranscode,
        item_title: &str,
        owner: SessionOwner<'_>,
    ) -> Option<StartInfo> {
        let cache = self.cache.as_ref()?;
        if !Self::plan_matches_request(plan, file, opts) {
            tracing::error!(
                target: "plurxd::transcode",
                file_id = file.id,
                "cache lookup refused: the plan does not describe this request"
            );
            return None;
        }
        let digest = self.digest()?;
        let hash = self.effective_recipe(&digest, plan, false).hash();
        let session_id = uuid::Uuid::new_v4().to_string();
        let (mut cache_location, mut cache_root) =
            match self.cache_read_location(&hash, cache).await {
                Ok(Some(hit)) => hit,
                other => {
                    // The name is logged on a miss because "why is this not
                    // hitting?" is otherwise unanswerable from outside: the hash
                    // is a pure function of a dozen inputs, and a producer and a
                    // player disagreeing about any one of them looks identical to
                    // an empty cache. With the name in both logs the disagreement
                    // is one `grep` rather than a bisect.
                    if let Err(e) = other {
                        tracing::warn!(
                            target: "plurxd::transcode",
                            recipe = %hash, error = %e, "cache lookup failed"
                        );
                    }
                    tracing::debug!(
                        target: "plurxd::transcode",
                        recipe = %hash, file = file.id, "transcode cache miss"
                    );
                    return None;
                }
            };
        let mut prepared_shared = None;
        if cache_location.storage_class == "shared" {
            let prepared = match (
                self.shared_cache.as_ref(),
                cache_location.manifest_digest.clone(),
            ) {
                (Some(coordinator), Some(expected_manifest)) => {
                    Self::prepare_shared_cached_read(
                        Arc::clone(&self.store),
                        Arc::clone(coordinator),
                        cache_root.clone(),
                        cache_location.clone(),
                        expected_manifest,
                        session_id.clone(),
                    )
                    .await
                }
                _ => Err("shared cache generation has no admitted manifest".to_owned()),
            };
            match prepared {
                Ok(prepared) => prepared_shared = Some(prepared),
                Err(error) => {
                    if let Some(shared_cache) = self.shared_cache.as_ref() {
                        shared_cache
                            .report_io_failure("shared_serve_preflight_failed")
                            .await;
                    }
                    tracing::warn!(target: "plurxd::transcode", recipe = %hash, %error, "shared cache read failed; trying node-local cache");
                    let local = match self.local_cache_read_location(&hash, cache).await {
                        Ok(Some(local)) => local,
                        Ok(None) => return None,
                        Err(error) => {
                            tracing::warn!(target: "plurxd::transcode", recipe = %hash, %error, "local cache fallback lookup failed");
                            return None;
                        }
                    };
                    (cache_location, cache_root) = local;
                }
            }
        }
        // Local deletion safety is process-local. Shared generations instead
        // need a durable exact-generation pin before the first filesystem
        // read, otherwise GC can retire the row between lookup and owner
        // publication. The short lookup pin bridges to the durable media
        // session pin installed before the route is exposed.
        let cache_lookup = if cache_location.storage_class == "shared" {
            prepared_shared.as_ref()?;
            None
        } else {
            let Some(guard) = self.cache_readers.begin_lookup(&hash) else {
                tracing::debug!(
                    target: "plurxd::transcode",
                    recipe = %hash, file = file.id, "cache entry is being evicted"
                );
                return None;
            };
            Some(guard)
        };
        let (dir, cache_manifest) = if let Some(prepared) = prepared_shared.take() {
            (prepared.dir, Some(prepared.manifest))
        } else {
            let Some(dir) =
                crate::cachekeep::validated_entry_dir(&cache_root, &cache_location.relative_dir)
                    .await
            else {
                self.invalidate_cache_location(&cache_location, "unsafe_relative_path")
                    .await;
                return None;
            };
            // The row says the bytes are there; the disk is what actually has
            // to have them. A cache root on a mount that did not come back
            // after a reboot would otherwise serve a playlist for an empty
            // directory — the row survives what the filesystem does not.
            if tokio::fs::metadata(dir.join("index.m3u8")).await.is_err() {
                tracing::warn!(
                    target: "plurxd::transcode",
                    recipe = %hash,
                    dir = %dir.display(),
                    "cache row points at a directory with no playlist — treating as a miss"
                );
                self.invalidate_cache_location(&cache_location, "playlist_missing")
                    .await;
                return None;
            }
            let cache_manifest = if let Some(expected) = cache_location.manifest_digest.as_deref() {
                let manifest_path = dir.join(plurx_core::transcode::manifest::MANIFEST_FILE);
                if tokio::fs::metadata(&manifest_path).await.is_err() {
                    tracing::warn!(
                        target: "plurxd::transcode",
                        recipe = %hash,
                        dir = %dir.display(),
                        "cache location has a fenced manifest digest but no manifest — treating as a miss"
                    );
                    self.invalidate_cache_location(&cache_location, "manifest_missing")
                        .await;
                    return None;
                }
                match crate::manifest_cache::load(
                    crate::manifest_cache::GenerationKey {
                        cache_root: cache_root.clone(),
                        node_id: cache_location.node_id.clone(),
                        recipe_hash: hash.clone(),
                        storage_class: cache_location.storage_class.clone(),
                        relative_dir: cache_location.relative_dir.clone(),
                        manifest_digest: expected.to_owned(),
                    },
                    &dir,
                )
                .await
                {
                    Ok(manifest) => Some(manifest),
                    Err(error) => {
                        tracing::warn!(
                            target: "plurxd::transcode",
                            recipe = %hash,
                            dir = %dir.display(),
                            %error,
                            "cache generation manifest is invalid — treating as a miss"
                        );
                        self.invalidate_cache_location(&cache_location, "manifest_invalid")
                            .await;
                        return None;
                    }
                }
            } else {
                // Legacy rows predate fenced manifests. A stray manifest may
                // be a losing queue adoption, so bounded legacy reads remain
                // the only authority until a fenced completion installs its
                // digest.
                None
            };
            let playlist_bytes = match &cache_manifest {
                Some(manifest) => manifest
                    .read_verified_playlist(&dir, "index.m3u8")
                    .await
                    .ok()
                    .flatten(),
                None => plurx_core::transcode::manifest::read_bounded_playlist(&dir, "index.m3u8")
                    .await
                    .ok()
                    .flatten(),
            };
            let playlist_valid = playlist_bytes
                .as_deref()
                .and_then(|bytes| std::str::from_utf8(bytes).ok())
                .and_then(validated_vod_part)
                .is_some();
            if !playlist_valid {
                self.invalidate_cache_location(&cache_location, "playlist_invalid_vod")
                    .await;
                return None;
            }
            (dir, cache_manifest)
        };
        let cache_reader = if cache_location.storage_class == "shared" {
            None
        } else {
            Some(self.cache_readers.begin_playback(&hash)?)
        };
        drop(cache_lookup);
        if let Some(generation_id) = cache_location.generation_id.as_deref() {
            // Best-effort recency hint: playback already pinned the generation
            // and a later read can refresh its last-access timestamp.
            crate::store_result::observe(
                crate::store_result::Operation::TouchSharedCacheEntry,
                crate::store_result::Discard::BestEffort,
                self.store
                    .touch_shared_cache_entry(
                        &hash,
                        &cache_location.node_id,
                        generation_id,
                        unix_ms(),
                    )
                    .await,
            );
        } else {
            // Best-effort recency hint: the selected cache object remains
            // usable, and a later read can refresh its access timestamp.
            crate::store_result::observe(
                crate::store_result::Operation::TouchCacheEntry,
                crate::store_result::Discard::BestEffort,
                self.store
                    .touch_cache_entry(&hash, &cache_location.node_id)
                    .await,
            );
        }
        let cached_kind = SessionKind::Transcode {
            height: opts.target_height,
        };
        let cached_codecs = transcoded_hls_codecs(opts.pipeline.output_grade(), opts.target_height);
        let cached_probe_json = self.store.get_file_probe_json(file.id).await.ok().flatten();
        let frozen_presentation = FrozenHlsPresentation::new(
            file.clone(),
            HlsContext {
                file_id: file.id,
                start_seconds: 0.0,
                media_origin_seconds: 0.0,
                codecs: cached_codecs.clone(),
                supplemental_codecs: None,
                frame_rate: frozen_video_frame_rate(cached_probe_json.as_deref()),
            },
            &cached_kind,
        );
        let control = crate::playback_control::RollingControlHandle::spawn("session-start");
        let failed = Arc::new(AtomicBool::new(false));
        if let Err(reason) = control
            .bind_response_publication_contract(
                frozen_presentation.contract_fingerprint.clone(),
                Arc::clone(&failed),
            )
            .await
        {
            tracing::warn!(
                target: "plurxd::transcode",
                session = %session_log_id(&session_id),
                rejection = ?reason,
                "rolling actor rejected cached response-publication ownership"
            );
            return None;
        }
        let session = Arc::new(Session {
            dir,
            response_incarnation: uuid::Uuid::new_v4(),
            frozen_presentation: Some(frozen_presentation),
            actor_managed_response_publication: true,
            actor_managed_prepublication_process: false,
            actor_prepublication_producer: Arc::new(AtomicBool::new(false)),
            response_publication_transition: Mutex::new(()),
            first_media_handoff_applied: AtomicBool::new(false),
            first_media_handoff_notify: tokio::sync::Notify::new(),
            prepublication_cleanup_active: AtomicBool::new(false),
            retirement_cleanup_started: AtomicBool::new(false),
            retirement_cleanup_finished: AtomicBool::new(false),
            retirement_settlement: std::sync::Mutex::new(None),
            scratch_cleanup_started: AtomicBool::new(false),
            retirement_context: Some(self.rolling_retirement_context()),
            cache_integrity_cleanup_started: AtomicBool::new(false),
            child: Mutex::new(None),
            child_transition: Mutex::new(()),
            replacing_child: AtomicBool::new(false),
            #[cfg(test)]
            replacement_pause: std::sync::Mutex::new(None),
            #[cfg(test)]
            activity_detail_pause: std::sync::Mutex::new(None),
            #[cfg(test)]
            control_applied_pause: std::sync::Mutex::new(None),
            terminal_response_pending: Arc::new(AtomicBool::new(false)),
            terminal_control: std::sync::Mutex::new(None),
            #[cfg(test)]
            flow_completion_pause: std::sync::Mutex::new(None),
            #[cfg(test)]
            playlist_publication_pause: std::sync::Mutex::new(None),
            #[cfg(test)]
            producer_install_pause: std::sync::Mutex::new(None),
            #[cfg(test)]
            refresh_after_read_pause: std::sync::Mutex::new(None),
            #[cfg(test)]
            path_owner_sample_pause: std::sync::Mutex::new(None),
            #[cfg(test)]
            retention_delete_pause: std::sync::Mutex::new(None),
            #[cfg(test)]
            response_projection_pause: std::sync::Mutex::new(None),
            #[cfg(test)]
            first_media_owner_claim_pause: std::sync::Mutex::new(None),
            #[cfg(test)]
            retirement_started: AtomicBool::new(false),
            #[cfg(test)]
            retirement_cleanup_handoff_pause: std::sync::Mutex::new(None),
            #[cfg(test)]
            scratch_cleanup_pause: std::sync::Mutex::new(None),
            cached: true,
            _cache_reader: cache_reader,
            subtitle_handle: None,
            #[cfg(windows)]
            source_handle: None,
            #[cfg(windows)]
            output_handle: None,
            cache_manifest,
            cache_location: Some(cache_location),
            control,
            publication: Mutex::new(RollingPublicationClock::default()),
            publication_worker_started: AtomicBool::new(false),
            flow_worker_started: AtomicBool::new(false),
            file_id: file.id,
            item_id: file.item_id,
            item_title: item_title.to_owned(),
            user_name: owner.user_name.to_owned(),
            supersession_user: owner.supersession_user.to_owned(),
            playback_id: owner.playback_id.to_owned(),
            // A cached serve has no producer, so it cannot fault and cannot
            // spend a budget.
            recovery: None,
            automatic: owner.automatic,
            kind: cached_kind,
            // A cache hit only ever answers a transcode request (`serve_cached`
            // is reached from the transcode path alone); the encoder label goes to
            // "cached" here, which is exactly why the method is not read off it.
            method: crate::delivery::Method::Transcode,
            start_seconds: 0.0,
            // A cache hit is the whole stream from the beginning.
            media_origin_seconds: 0.0,
            grade: opts.pipeline.output_grade(),
            target_height: opts.target_height,
            tone_map_peak_nits: (plan.options().tone_map == ToneMap::Zscale)
                .then_some(plan.options().tone_map_peak_nits),
            tone_map_peak_source: (plan.options().tone_map == ToneMap::Zscale)
                .then_some(plan.options().tone_map_peak_source.name()),
            encoder_label: Mutex::new("cached"),
            started_unix: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
            failed,
            failure: std::sync::Mutex::new(None),
            playlist_published: AtomicBool::new(true),
            high_segment: Arc::new(AtomicI64::new(-1)),
            compatibility_attempt: Arc::new(std::sync::Mutex::new(0)),
            fetched_end_ms: Arc::new(AtomicI64::new(0)),
            segments: Mutex::new(SegmentIndex::default()),
            ahead_bytes: AtomicI64::new(0),
            live_bytes: Arc::new(AtomicI64::new(0)),
            scratch: None,
            retired_release: Arc::new(RetiredRelease::new()),
            scratch_envelope: 0,
            upload: None,
            retention_garbage_bytes: Arc::new(AtomicI64::new(0)),
            retention_cleanup_queue: Arc::new(std::sync::Mutex::new(Vec::new())),
            retention_cleanup_active: Arc::new(AtomicBool::new(false)),
            progress: Arc::new(Progress::new()),
            class: std::sync::Mutex::new(String::new()),
            hw_slot: std::sync::Mutex::new(None),
            sw_permit: std::sync::Mutex::new(None),
            sw_delta_permit: std::sync::Mutex::new(None),
            delivery: Meter::for_method(crate::delivery::Method::Transcode.metric_label()),
            http_waits: HttpWaitLedger::default(),
            readrate: 0.0,
            suspended: AtomicBool::new(false),
            suspended_at: Mutex::new(None),
            suspend_count: AtomicU64::new(0),
            takeover: None,
            first_slide_logged: AtomicBool::new(false),
        });
        if let Err(reason) = self
            .register_session(&session_id, Arc::clone(&session), 0)
            .await
        {
            tracing::warn!(
                target: "plurxd::transcode",
                session = %session_log_id(&session_id),
                rejection = ?reason,
                "cached rolling session registration rejected"
            );
            return None;
        }
        tracing::info!(
            target: "plurxd::transcode",
            session = %session_log_id(&session_id), recipe = %hash, file = file.id,
            "serving a cached transcode — no encoder started"
        );
        self.emit_session_event(
            &session_id,
            &session,
            "session_start",
            SessionEventFields {
                extra: Some(serde_json::json!({ "cache": "hit" }).to_string()),
                ..SessionEventFields::default()
            },
        )
        .await;
        Some(StartInfo {
            playlist_url: format!("/api/v1/hls/{session_id}/index.m3u8"),
            session_id,
            duration_ms: file.duration_ms,
            // A cached asset is the whole title, so playback starts at zero and
            // the player seeks into it. `start_seconds` on a live session
            // exists because the encoder had to be told where to begin; here
            // there is no encoder and nothing to tell.
            start_seconds: 0.0,
            media_origin_seconds: 0.0,
            target_height: opts.target_height,
            kind: SessionKind::Transcode {
                height: opts.target_height,
            },
            encoder: "cached",
            grade: opts.pipeline.output_grade(),
            vod: true,
            control_lease_timeout_ms: crate::playback_control::ROLLING_LEASE_TIMEOUT_MS,
        })
    }

    // ---- the pre-transcode producer (PERF-PLAN §6.2) -----------------------
}
