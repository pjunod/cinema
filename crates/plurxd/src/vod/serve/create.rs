use super::*;

/// In-process preparation provenance, never a native/refund capability.
#[cfg(target_os = "linux")]
pub(crate) struct StaticArtifactAdmissionError {
    message: String,
    definitely_unreserved: bool,
    cache_grant: Option<crate::playback_cache::FiniteCacheReservation>,
    compatibility: Option<crate::vodencode::FiniteSoftwareCompatibility>,
}
#[cfg(target_os = "linux")]
impl StaticArtifactAdmissionError {
    pub(crate) fn into_parts(
        self,
    ) -> (
        String,
        bool,
        Option<crate::playback_cache::FiniteCacheReservation>,
        Option<crate::vodencode::FiniteSoftwareCompatibility>,
    ) {
        (
            self.message,
            self.definitely_unreserved,
            self.cache_grant,
            self.compatibility,
        )
    }
}

/// One request's resolved native recipe, before any rendition is attached.
struct PreparedVodRecipe {
    identity: SourceIdentity,
    index: Option<FragmentIndex>,
    recipe: Recipe,
    duration_ms: i64,
    incoming_logical: Option<crate::vodserve::retained_manifest::LogicalOutput>,
}

#[cfg(target_os = "linux")]
pub(crate) struct PreparedAdaptiveRecipe {
    key: String,
    identity: SourceIdentity,
    index: Option<FragmentIndex>,
    recipe: Recipe,
    duration_ms: i64,
}

#[cfg(target_os = "linux")]
impl PreparedAdaptiveRecipe {
    /// Actual requested capacities of the recipe held between P closure and
    /// R admission. The M handoff partition must fit this with the original
    /// coordinator envelope before `build_adaptive_recipe` enters native work.
    pub(crate) fn requested_heap_bytes(&self) -> Option<usize> {
        let mut bytes = std::mem::size_of::<Self>()
            .checked_add(self.key.capacity())?
            .checked_add(self.identity.argv_fingerprint.capacity())?
            .checked_add(crate::vodserve::finite_metadata::recipe_heap_bytes(
                &self.recipe,
            )?)?;
        if let Some(index) = &self.index {
            bytes = bytes.checked_add(index.requested_heap_bytes()?)?;
        }
        Some(bytes)
    }

    pub(crate) fn key_capacity(&self) -> usize {
        self.key.capacity()
    }

    /// Serialize the locally selected route in the format all existing route
    /// readers and recovery paths understand. This is a Store equality fact,
    /// not an authority to acquire a source or prove physical closure.
    pub(crate) fn selected_recipe_json(
        &self,
        envelope: &crate::media_sessions::RemoteStartRequest,
    ) -> Result<String, String> {
        let planned_source = self
            .recipe
            .encoding
            .as_ref()
            .and_then(|encoding| encoding.planning_metadata.as_ref());
        if !envelope.is_valid()
            || envelope.request.file_id != self.recipe.file.id
            || envelope.source_size != self.recipe.file.size
            || envelope.source_mtime != self.recipe.file.mtime
            || !planned_source.is_some_and(|source| {
                source.source_file_id().parse::<i64>() == Ok(self.recipe.file.id)
                    && source.scanner_size() == self.recipe.file.size
                    && source.scanner_mtime() == self.recipe.file.mtime
                    && self.recipe.source_object_version.as_deref()
                        == Some(source.source_object_version())
            })
            || !finite_software::selected_recipe(&self.recipe, false, false)
            || rendition_key(&self.recipe, &self.identity) != self.key
        {
            return Err("adaptive selected route differs from its local finite recipe".into());
        }
        // The Store's selected-route limit is 32 KiB. Reserve and verify
        // that one actual backing allocation before serialization; a writer
        // error refuses an overlong route before Vec can grow past it.
        struct BoundedRoute(Vec<u8>);
        impl std::io::Write for BoundedRoute {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if bytes.len() > 32 * 1024 - self.0.len() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "adaptive selected route exceeds Store limit",
                    ));
                }
                self.0.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(32 * 1024)
            .map_err(|_| "reserving adaptive selected route capacity")?;
        if bytes.capacity() > 32 * 1024 {
            return Err("adaptive selected route allocation exceeds its charged limit".into());
        }
        let mut writer = BoundedRoute(bytes);
        serde_json::to_writer(&mut writer, envelope)
            .map_err(|error| format!("serializing adaptive selected route: {error}"))?;
        let json =
            String::from_utf8(writer.0).map_err(|_| "adaptive selected route is not UTF-8")?;
        plurx_core::store::adaptive_resource_attempts::selected_recipe_digest(&json)
            .map_err(|error| format!("adaptive selected route exceeds Store bounds: {error}"))?;
        Ok(json)
    }
}

/// Private publication/storage identity; the canonical recipe remains the
/// source of candidate identity and the stored full-title plan.
fn processed_instance_key(canonical_key: &str, incarnation: &str, nonce: [u8; 16]) -> String {
    let mut hash = Sha256::new();
    hash.update(b"plurx:processed-vod-instance:v1\0");
    hash.update(canonical_key.as_bytes());
    hash.update(incarnation.as_bytes());
    hash.update(crate::ffmpeg::encoded_process_identity().as_bytes());
    hash.update(nonce);
    hex::encode(hash.finalize())
}

impl VodServe {
    /// Finite background obligation in the existing rendition driver. This
    /// never registers a session, reader, wait-pool demand or synthetic GET.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn prepare_copy_output(
        &self,
        prepared: VodRecipeRequest<'_>,
        file: &MediaFile,
        settings: &VodSettings,
        source_version: &str,
        cap: u64,
        fence: crate::background_jobs::JobFence,
        deadline: Instant,
        admissions: crate::admission::Admissions,
        media_engine: (
            Arc<crate::ffmpeg::EncodedExecutable>,
            crate::ffmpeg::EncodedEngine,
        ),
        still_idle: impl Fn() -> bool,
    ) -> Result<super::copy_preparation::PreparedCopyOutput, crate::background_jobs::PreparationError>
    {
        if prepared.encoding.is_some() {
            return Err(crate::background_jobs::PreparationError::Fail(
                "copy_preparation_with_encoding",
            ));
        }
        self.prepare_complete_output(
            prepared,
            file,
            settings,
            source_version,
            cap,
            fence,
            deadline,
            admissions,
            media_engine,
            still_idle,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn prepare_encoded_output(
        &self,
        prepared: VodRecipeRequest<'_>,
        file: &MediaFile,
        settings: &VodSettings,
        source_version: &str,
        cap: u64,
        fence: crate::background_jobs::JobFence,
        deadline: Instant,
        admissions: crate::admission::Admissions,
        media_engine: (
            Arc<crate::ffmpeg::EncodedExecutable>,
            crate::ffmpeg::EncodedEngine,
        ),
        still_idle: impl Fn() -> bool,
    ) -> Result<super::copy_preparation::PreparedCopyOutput, crate::background_jobs::PreparationError>
    {
        if prepared.encoding.is_none() {
            return Err(crate::background_jobs::PreparationError::Fail(
                "encoded_preparation_without_plan",
            ));
        }
        self.prepare_complete_output(
            prepared,
            file,
            settings,
            source_version,
            cap,
            fence,
            deadline,
            admissions,
            media_engine,
            still_idle,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn prepare_complete_output(
        &self,
        mut prepared: VodRecipeRequest<'_>,
        file: &MediaFile,
        settings: &VodSettings,
        source_version: &str,
        cap: u64,
        fence: crate::background_jobs::JobFence,
        deadline: Instant,
        admissions: crate::admission::Admissions,
        media_engine: (
            Arc<crate::ffmpeg::EncodedExecutable>,
            crate::ffmpeg::EncodedEngine,
        ),
        still_idle: impl Fn() -> bool,
    ) -> Result<super::copy_preparation::PreparedCopyOutput, crate::background_jobs::PreparationError>
    {
        use crate::background_jobs::PreparationError;
        if prepared.root_admission.is_some() {
            return Err(PreparationError::Fail(
                "foreground_preparation_not_persistable",
            ));
        }
        let expected_encoded_plan = prepared
            .encoding
            .as_ref()
            .map(|encoding| encoding.plan.plan_digest().to_owned());
        let attachment_observation = *self
            .shared
            .preparation_attachment
            .lock()
            .expect("attachment observation");
        if !still_idle() {
            return Err(PreparationError::Yield("preempted"));
        }
        if Instant::now() >= deadline {
            return Err(PreparationError::Yield("pass_deadline"));
        }
        // A full retained budget is refused before any media work starts, so
        // queueing again costs one reservation check, never a remux/encode.
        let allowance = super::retained::RetainedArtifactRegistry::reserve_preparation(
            &self.shared,
            cap,
            settings.output_budget_bytes,
        )
        .await
        .ok_or(PreparationError::Yield("retention_capacity"))?;
        let mut storage_run =
            super::preparation_storage::ConstructionOwner::new(Arc::clone(&allowance));
        let (attachment, logical) = self
            .resolve_rendition(
                &mut prepared,
                file,
                settings,
                None,
                Some(Arc::clone(&allowance)),
                None,
            )
            .await?;
        let rendition = Arc::clone(&attachment.rendition);
        let logical = logical.ok_or("copy preparation logical facts unavailable")?;
        self.shared.hooks.get().before_preparation_snapshot().await;
        let token = fence
            .snapshot()
            .await
            .ok_or(PreparationError::Yield("lease_lost"))?;
        let readers = rendition.readers.lock().await;
        let manifest = rendition.manifest.lock().await;
        if !rendition
            .source
            .as_ref()
            .is_some_and(|source| source.unchanged() && source.object_version() == source_version)
        {
            return Err(PreparationError::Stop("source_changed"));
        }
        if rendition.recipe.retained_logical.as_ref() != Some(&logical) {
            return Err(PreparationError::Fail("retained_logical_changed"));
        }
        if !still_idle()
            || Instant::now() >= deadline
            || !readers.is_empty()
            || manifest.materialized_count() != 0
            || rendition.preparation().is_some()
        {
            // A viewer or another preparation already owns this rendition.
            return Err(PreparationError::Yield("rendition_busy"));
        }
        let preparation = Arc::new(super::copy_preparation::CopyPreparation::new(
            allowance,
            fence,
            token,
            deadline,
            logical,
            source_version.to_owned(),
            expected_encoded_plan,
            admissions,
            media_engine,
            attachment_observation,
        ));
        // Reserve metadata separately from media's RFC numerator and horizon
        // charge. The manifest writer itself is bounded by MAX_MANIFEST.
        let metadata = preparation
            .allowance
            .begin(
                super::retained_manifest::MAX_MANIFEST
                    .checked_add(rendition.playlist.len() as u64)
                    .ok_or(PreparationError::Fail("preparation_metadata_overflow"))?,
            )
            .ok_or(PreparationError::Fail("preparation_metadata_exceeds_cap"))?;
        metadata.commit(false);
        {
            let observation = self
                .shared
                .preparation_attachment
                .lock()
                .expect("attachment observation");
            if *observation != attachment_observation || *observation == u64::MAX {
                return Err(PreparationError::Yield("preempted"));
            }
            *rendition.copy_preparation.lock().expect("copy preparation") =
                Some(Arc::clone(&preparation));
        }
        let run = super::copy_preparation::PreparationRun {
            rendition: Arc::clone(&rendition),
            preparation,
            armed: true,
        };
        drop(manifest);
        drop(readers);
        storage_run.disarm(); // PreparationRun now owns cancellation settlement.
        drop(attachment); // Never hold the attachment gate for full-film work.
        rendition.kick();
        self.wait_prepared_copy(run, still_idle).await
    }

    /// The VOD arm of session create, called by the manager AFTER it has
    /// decided the request opts in (`presentation=="vod" && settings.enabled`).
    ///
    /// Registers a live session handle attached to a (created or resurrected)
    /// rendition and kicks its producer driver. A request that cannot be
    /// VOD-presented returns a stable refusal; it never changes presentation.
    /// `start_seconds` positions the first demand (the entry containing it),
    /// not the plan.
    #[cfg(test)]
    pub(crate) async fn try_create<'a>(
        &self,
        req: impl Into<VodRecipeRequest<'a>>,
        file: &MediaFile,
        settings: &VodSettings,
        attribution: VodAttribution<'_>,
        session_id: String,
    ) -> Result<VodStart, String> {
        self.try_create_with_release_fence(
            req.into(),
            file,
            settings,
            attribution,
            session_id,
            VodCreateFences {
                release_fence: None,
                serving_admission: None,
                viewer: None,
                expected_attempt: None,
            },
        )
        .await
    }

    /// Cluster-only VOD creation. Unlike the legacy local entrypoint, this
    /// carries a serving admission captured before preparation and fences the
    /// final attachment against the corresponding quorum-loss transition.
    #[cfg(test)]
    pub(crate) async fn try_create_cluster<'a>(
        &self,
        req: impl Into<VodRecipeRequest<'a>>,
        file: &MediaFile,
        settings: &VodSettings,
        attribution: VodAttribution<'_>,
        session_id: String,
        serving_admission: VodServingAdmission,
    ) -> Result<VodStart, String> {
        self.try_create_with_release_fence(
            req.into(),
            file,
            settings,
            attribution,
            session_id,
            VodCreateFences {
                release_fence: None,
                serving_admission: Some(serving_admission),
                viewer: None,
                expected_attempt: None,
            },
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn try_create_cluster_for_viewer<'a>(
        &self,
        req: impl Into<VodRecipeRequest<'a>>,
        file: &MediaFile,
        settings: &VodSettings,
        attribution: VodAttribution<'_>,
        session_id: String,
        serving_admission: VodServingAdmission,
        viewer: crate::state::PlaybackViewerDemand,
    ) -> Result<VodStart, String> {
        self.try_create_with_release_fence(
            req.into(),
            file,
            settings,
            attribution,
            session_id,
            VodCreateFences {
                release_fence: None,
                serving_admission: Some(serving_admission),
                viewer: Some(viewer),
                expected_attempt: None,
            },
        )
        .await
    }

    pub(crate) async fn try_create_for_viewer<'a>(
        &self,
        req: impl Into<VodRecipeRequest<'a>>,
        file: &MediaFile,
        settings: &VodSettings,
        attribution: VodAttribution<'_>,
        session_id: String,
        viewer: crate::state::PlaybackViewerDemand,
    ) -> Result<VodStart, String> {
        self.try_create_with_release_fence(
            req.into(),
            file,
            settings,
            attribution,
            session_id,
            VodCreateFences {
                release_fence: None,
                serving_admission: None,
                viewer: Some(viewer),
                expected_attempt: None,
            },
        )
        .await
    }

    /// Attach the already installed original adaptive attempt after its
    /// exact Store Adopt. The selected rendition is pinned by Arc identity;
    /// a changed key or lost owner refuses before any replacement build.
    #[cfg(target_os = "linux")]
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn try_create_adopted_attempt<'a>(
        &self,
        req: VodRecipeRequest<'a>,
        file: &MediaFile,
        settings: &VodSettings,
        attribution: VodAttribution<'_>,
        session_id: String,
        serving_admission: VodServingAdmission,
        viewer: crate::state::PlaybackViewerDemand,
        pending: &'a shared::PreparedAttemptRendition,
    ) -> Result<VodStart, String> {
        self.try_create_with_release_fence(
            req,
            file,
            settings,
            attribution,
            session_id,
            VodCreateFences {
                release_fence: None,
                serving_admission: Some(serving_admission),
                viewer: Some(viewer),
                expected_attempt: Some(pending.rendition()),
            },
        )
        .await
    }

    /// Prepare a resurrection normally, then serialize only its final
    /// lifecycle/reader/registry attachment against public release. The
    /// release bit is checked while that exact transition is held, so slow
    /// Store/index/rendition work never delays a DELETE tombstone.
    pub(crate) async fn try_create_before_release<'a>(
        &self,
        req: impl Into<VodRecipeRequest<'a>>,
        file: &MediaFile,
        settings: &VodSettings,
        attribution: VodAttribution<'_>,
        session_id: String,
        release_fence: VodReleaseFence<'_>,
    ) -> Result<VodStart, String> {
        let _preparing = self.begin_preparing_session(&session_id);
        self.try_create_with_release_fence(
            req.into(),
            file,
            settings,
            attribution,
            session_id,
            VodCreateFences {
                release_fence: Some(release_fence),
                serving_admission: None,
                viewer: None,
                expected_attempt: None,
            },
        )
        .await
    }

    pub(crate) fn begin_preparing_session(&self, session_id: &str) -> VodPreparationGuard {
        *self
            .shared
            .preparing_sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(session_id.to_owned())
            .or_insert(0) += 1;
        VodPreparationGuard {
            shared: Arc::clone(&self.shared),
            session_id: session_id.to_owned(),
        }
    }

    pub(crate) async fn owns_or_preparing(&self, session_id: &str) -> bool {
        if self
            .shared
            .preparing_sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(session_id)
        {
            return true;
        }
        self.shared.sessions.lock().await.contains_key(session_id)
    }

    pub(crate) async fn live_or_preparing_session_ids(&self) -> Vec<String> {
        let mut ids = self
            .shared
            .sessions
            .lock()
            .await
            .iter()
            .filter(|(_, session)| session.renewable())
            .map(|(session_id, _)| session_id.clone())
            .collect::<Vec<_>>();
        ids.extend(
            self.shared
                .preparing_sessions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .keys()
                .cloned(),
        );
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    /// Resolve immutable media without attaching a public playback handle.
    /// The returned build guard protects the lookup/reader-attachment gap;
    /// callers must commit their parent-owned reader graph before dropping it.
    pub(super) async fn resolve_rendition(
        &self,
        prepared: &mut VodRecipeRequest<'_>,
        file: &MediaFile,
        settings: &VodSettings,
        viewer: Option<&crate::state::PlaybackViewerDemand>,
        private_preparation: Option<Arc<super::copy_preparation::PreparationAllowance>>,
        expected_attempt: Option<&Arc<Rendition>>,
    ) -> Result<
        (
            RenditionAttachment,
            Option<crate::vodserve::retained_manifest::LogicalOutput>,
        ),
        String,
    > {
        self.shared.pool.set_global_cap(settings.blocked_get_cap);
        let PreparedVodRecipe {
            identity,
            index,
            recipe,
            duration_ms,
            incoming_logical,
        } = self
            .prepare_recipe(prepared, file, settings, viewer)
            .await?;
        #[cfg(target_os = "linux")]
        let identity = if finite_software::selected_recipe(
            &recipe,
            private_preparation.is_some(),
            prepared
                .root_admission
                .as_ref()
                .and_then(|root| root.instance_nonce)
                .is_some(),
        ) {
            recipe
                .encoding
                .as_ref()
                .expect("selected finite recipe has an encoding")
                .finite_software_identity(&recipe.file, duration_ms as f64 / 1_000.0)?
        } else {
            identity
        };
        let canonical_key = rendition_key(&recipe, &identity);
        if let Some(reuse) = prepared
            .root_admission
            .as_ref()
            .and_then(|root| root.reused_parent.as_ref())
        {
            // Resolve only the captured physical graph. Even a same-key
            // cache replacement must never spend its predecessor's credit.
            let rendition = Arc::clone(&reuse.rendition);
            let gate = self
                .shared
                .rendition_build_gate(&rendition.key)
                .lock_owned()
                .await;
            let cached = self.shared.renditions.lock().await;
            if canonical_key != rendition_key(&rendition.recipe, &identity)
                || !cached
                    .get(&rendition.key)
                    .is_some_and(|current| Arc::ptr_eq(current, &rendition))
                || rendition.closed.load(Acquire)
            {
                return Err(crate::transcode::vod_refusal_error(
                    "vod_preparation_changed",
                    "the captured processing graph changed",
                ));
            }
            return Ok((
                RenditionAttachment {
                    rendition,
                    _build_guard: gate,
                },
                incoming_logical,
            ));
        }
        let processing_instance = prepared
            .root_admission
            .as_ref()
            .and_then(|admission| admission.instance_nonce);
        let key = if let Some(nonce) = processing_instance {
            let incarnation = prepared
                .request
                .request_id
                .as_deref()
                .ok_or("processed instance requires admitted incarnation")?;
            processed_instance_key(&canonical_key, incarnation, nonce)
        } else {
            private_preparation.as_ref().map_or_else(
                || canonical_key.clone(),
                |nonce| {
                    let mut hash = Sha256::new();
                    hash.update(b"plurx:private-copy-preparation-incarnation:v1\0");
                    hash.update(canonical_key.as_bytes());
                    hash.update(nonce.nonce.as_bytes());
                    hex::encode(hash.finalize())
                },
            )
        };
        if let Some(expected) = expected_attempt {
            if expected.key != key || expected.closed.load(Acquire) {
                return Err(
                    "adopted adaptive rendition no longer matches its selected recipe".into(),
                );
            }
            let gate = self.shared.rendition_build_gate(&key).lock_owned().await;
            let cached = self.shared.renditions.lock().await;
            if !cached
                .get(&key)
                .is_some_and(|current| Arc::ptr_eq(current, expected))
            {
                return Err("adopted adaptive rendition is no longer the original owner".into());
            }
            drop(cached);
            return Ok((
                RenditionAttachment {
                    rendition: Arc::clone(expected),
                    _build_guard: gate,
                },
                incoming_logical,
            ));
        }
        let attachment = self
            .shared
            .attach_rendition_with_storage(
                &key,
                &identity,
                index,
                recipe,
                duration_ms,
                settings,
                private_preparation,
                processing_instance.map(|_| {
                    (
                        canonical_key.as_str(),
                        prepared
                            .root_admission
                            .as_ref()
                            .and_then(|admission| admission.instance_allowance.clone()),
                    )
                }),
            )
            .await?
            .ok_or_else(|| {
                crate::transcode::vod_refusal_error(
                    "vod_source_unsupported",
                    "the fragment index produced an empty VOD plan",
                )
            })?;
        Ok((attachment, incoming_logical))
    }

    /// Resolve the ordinary selected recipe without admitting any native
    /// owner. The caller can now destroy its request/catalogue DTO before
    /// reserving the producer role; only this exact recipe moves forward.
    #[cfg(target_os = "linux")]
    pub(crate) async fn resolve_adaptive_recipe(
        &self,
        prepared: &mut VodRecipeRequest<'_>,
        file: &MediaFile,
        settings: &VodSettings,
    ) -> Result<PreparedAdaptiveRecipe, String> {
        if prepared.root_admission.is_some() {
            return Err("adaptive finite attempt cannot borrow a predecessor root".into());
        }
        let PreparedVodRecipe {
            identity: _,
            index,
            recipe,
            duration_ms,
            incoming_logical: _,
        } = self.prepare_recipe(prepared, file, settings, None).await?;
        if !finite_software::selected_recipe(&recipe, false, false) {
            return Err("adaptive attempt requires selected finite software AVC".into());
        }
        let identity = recipe
            .encoding
            .as_ref()
            .expect("selected recipe has a frozen encoding")
            .finite_software_identity(&recipe.file, duration_ms as f64 / 1_000.0)?;
        let key = rendition_key(&recipe, &identity);
        Ok(PreparedAdaptiveRecipe {
            key,
            identity,
            index,
            recipe,
            duration_ms,
        })
    }

    /// This second phase is entered only by the original registered attempt
    /// task, after request-only DTOs have been released. It keeps the result
    /// dormant until the Store's exact Adopt is known committed.
    #[cfg(target_os = "linux")]
    pub(crate) async fn build_adaptive_recipe(
        &self,
        prepared: PreparedAdaptiveRecipe,
        settings: &VodSettings,
        application_owner_bytes: usize,
    ) -> Result<shared::PreparedAttemptRendition, shared::AdaptiveBuildFailure> {
        let PreparedAdaptiveRecipe {
            key,
            identity,
            index,
            recipe,
            duration_ms,
        } = prepared;
        self.shared
            .prepare_adaptive_attempt(
                key,
                identity,
                index,
                recipe,
                duration_ms,
                settings,
                application_owner_bytes,
            )
            .await
    }

    /// Resolve the selected background artifact while the original P owner
    /// still covers pure plan/command allocations. This phase performs no R
    /// admission and returns no native authority.
    #[cfg(target_os = "linux")]
    pub(crate) async fn plan_static_artifact(
        &self,
        prepared: PreparedAdaptiveRecipe,
        policy: super::background_artifact::StaticArtifactPolicy,
        registry_owner_bytes: usize,
        planning_handoff_limit_bytes: usize,
        deadline: Instant,
    ) -> Result<super::background_artifact::StaticArtifactAdmission, String> {
        let PreparedAdaptiveRecipe {
            key,
            identity,
            index,
            recipe,
            duration_ms,
        } = prepared;
        if !finite_software::selected_recipe(&recipe, false, false) {
            return Err("static artifact requires the selected software AVC recipe".into());
        }
        let runtime = self
            .shared
            .resource_runtime
            .as_ref()
            .ok_or("static artifact resource runtime is unavailable")?;
        let plan = self
            .shared
            .stored_plan(&key, &identity, &index, &recipe, duration_ms)
            .await?;
        if plan.is_empty() || plan.len() > 8192 {
            return Err("static artifact has no bounded segment plan".into());
        }
        let playlist = plan.playlist().into_bytes();
        let logical = recipe
            .retained_logical
            .as_ref()
            .ok_or("static artifact has no resolved delivery identity")?
            .clone();
        let source_version = finite_profile::source_version(&recipe)?;
        let workspace = retained_manifest::projected_static_manifest_workspace_bytes(
            plan.len(),
            playlist.capacity(),
            logical
                .requested_heap_bytes()
                .ok_or("static delivery identity capacity overflow")?,
        )
        .and_then(|bytes| u64::try_from(bytes).ok())
        .ok_or("static manifest workspace capacity overflow")?;
        let publication = super::background_artifact::StaticArtifactPublication::new(
            key.clone(),
            source_version,
            plan.clone(),
            playlist,
            0,
        )
        .map_err(|error| format!("constructing static publication observer: {error}"))?;
        let application_owner_bytes =
            super::background_artifact::StaticArtifactRun::projected_application_bytes(
                &publication,
                &logical,
                registry_owner_bytes,
            )
            .ok_or("static application owner capacity overflow")?;
        let (_, software) =
            plurx_core::resource_settings::admission_pair(self.shared.store.as_ref())
                .await
                .map_err(|error| format!("reading static software pool policy: {error}"))?;
        let software_pool_threads = software
            .as_deref()
            .map(str::trim)
            .map(str::parse::<u64>)
            .transpose()
            .map_err(|_| "static software pool policy is invalid")?
            .unwrap_or_else(|| crate::admission::software_budget() as u64);
        let epoch = runtime
            .refresh_admission_capacity(deadline)
            .await
            .map_err(|error| format!("observing static artifact admission: {error:?}"))?;
        let accounting = runtime
            .accounting()
            .map_err(|error| format!("reading static artifact admission: {error:?}"))?;
        let mut capacity = accounting.capacity;
        capacity.cpu_ns_per_second = match capacity.cpu_ns_per_second {
            plurx_core::process::observation::Metric::Known(cpu) => {
                plurx_core::process::observation::Metric::Known(
                    cpu.saturating_sub(accounting.used.cpu_ns_per_second),
                )
            }
            unknown => unknown,
        };
        let (mut request, finalized_encoder_threads) = finite_profile::compile(
            &self.shared,
            &key,
            &recipe,
            index.as_ref(),
            &plan,
            epoch,
            capacity,
            software_pool_threads,
        )?;
        request = finite_profile::with_static_manifest_file(request)?;
        request
            .charge_application_owner_bytes(application_owner_bytes)
            .map_err(|error| format!("charging the static run owner: {error}"))?;
        let mut publication = publication;
        publication.set_epoch(epoch);
        // Same finalized request/policy as core below, expressed once in the
        // legacy compatibility pool for ordinary/selected mutual exclusion.
        // This is an alias, never another physical NodeResourceRuntime role.
        let compatibility = recipe
            .encoding
            .as_ref()
            .expect("selected static encoding")
            .admit_finite_compatibility(
                software_pool_threads,
                finalized_encoder_threads,
                true,
                deadline,
            )
            .await
            .ok_or("static software compatibility pool is occupied")?;
        let cache_grant = tokio::time::timeout_at(
            deadline.into(),
            crate::playback_cache::reserve_finite(
                &self.shared.base.join(&request.rendition_id),
                request.output.reserved_storage_bytes,
            ),
        )
        .await
        .map_err(|_| "static operator cache quota deadline elapsed".to_owned())?
        .map_err(|error| format!("reserving static operator cache quota: {error}"))?;
        let admission = super::background_artifact::StaticArtifactAdmission {
            request,
            cache_grant,
            compatibility,
            policy,
            workspace,
            publication,
            logical,
            application_owner_bytes,
        };
        if admission
            .requested_heap_bytes()
            .is_none_or(|bytes| bytes > planning_handoff_limit_bytes)
        {
            return Err("static artifact plan exceeds the admitted P application partition".into());
        }
        Ok(admission)
    }

    /// Called in the same coordinator poll that positively closes P. There
    /// is no await or fallible daemon allocation between core R admission and
    /// installation of its sole original run owner in the manager registry.
    #[cfg(target_os = "linux")]
    pub(crate) fn admit_static_artifact(
        &self,
        planned: super::background_artifact::StaticArtifactAdmission,
        deadline: Instant,
    ) -> Result<super::background_artifact::StaticArtifactRun, StaticArtifactAdmissionError> {
        let runtime =
            self.shared
                .resource_runtime
                .as_ref()
                .ok_or_else(|| StaticArtifactAdmissionError {
                    message: "static artifact resource runtime is unavailable".into(),
                    definitely_unreserved: true,
                    cache_grant: None,
                    compatibility: None,
                })?;
        let super::background_artifact::StaticArtifactAdmission {
            request,
            cache_grant,
            compatibility,
            policy,
            workspace,
            publication,
            logical,
            application_owner_bytes,
        } = planned;
        // Core takes the original application lease before accepted R and
        // installs its complete cleanup owner before exposing the preparation.
        let cache_grant = crate::playback_cache::FiniteCacheReservation::enter(cache_grant);
        let compatibility = compatibility.enter();
        let (owner, application_lease) = match runtime.prepare_static_software_artifact(
            request,
            workspace,
            application_owner_bytes,
            deadline,
        ) {
            Ok(owners) => owners,
            Err(error) => {
                let definitely_unreserved = error.definitely_before_reservation();
                let (cache_grant, compatibility) = if definitely_unreserved {
                    cache_grant.known_closed();
                    compatibility.known_retired();
                    (None, None)
                } else {
                    (Some(cache_grant), Some(compatibility))
                };
                return Err(StaticArtifactAdmissionError {
                    definitely_unreserved,
                    cache_grant,
                    compatibility,
                    message: format!("admitting the static artifact graph: {error}"),
                });
            }
        };
        Ok(super::background_artifact::StaticArtifactRun::new(
            owner,
            cache_grant,
            compatibility,
            publication,
            logical,
            application_lease,
            policy,
        ))
    }

    /// Select the output once for this attachment. New capture and issued
    /// recovery stay distinct; viewer start positions never trim full artifacts.
    pub(super) async fn capture_retained_output(
        &self,
        capture: RetainedOutputCapture,
        rendition: &Arc<Rendition>,
        incoming_logical: &Option<crate::vodserve::retained_manifest::LogicalOutput>,
        req: &SessionRequest,
        file: &MediaFile,
        continuous: bool,
    ) -> Result<Option<Arc<crate::vodserve::retained::RetainedVodArtifact>>, String> {
        #[cfg(target_os = "linux")]
        let issued_restore = matches!(&capture, RetainedOutputCapture::Restore(_));
        let selected = match capture {
            RetainedOutputCapture::Restore(Some(ref expected)) => Some(
                self.shared
                    .retained_artifacts
                    .reacquire_expected_for_request(
                        expected,
                        &self.shared,
                        rendition,
                        incoming_logical,
                        rendition.materialize_budget,
                    )
                    .await
                    .ok_or_else(|| {
                        crate::transcode::vod_refusal_error(
                            "retained_artifact_unavailable",
                            "the issued output artifact cannot be exactly reacquired",
                        )
                    })?,
            ),
            RetainedOutputCapture::Restore(None) | RetainedOutputCapture::ReceiverUnavailable => {
                None
            }
            // A continuous parent serves role-split private children; no
            // complete retained output can stand in for that reader graph.
            RetainedOutputCapture::New if continuous => None,
            RetainedOutputCapture::New => {
                let existing = rendition
                    .output_measurement
                    .lock()
                    .expect("output measurement lock")
                    .complete_rates()
                    .and_then(|rates| self.shared.retained_artifacts.acquire(&rates.identity))
                    .filter(|artifact| artifact.logical == *incoming_logical);
                if existing.is_some() {
                    existing
                } else if req.candidate_context.is_some() {
                    self.shared
                        .retained_artifacts
                        .acquire_prepared_candidate(rendition, incoming_logical, file, req)
                        .await
                } else {
                    self.shared
                        .retained_artifacts
                        .acquire_prepared_manual(rendition, incoming_logical, file)
                        .await
                }
            }
        };
        #[cfg(target_os = "linux")]
        if let Some(artifact) = selected.as_ref() {
            if !artifact
                .static_policy_current(self.shared.store.as_ref())
                .await?
            {
                self.shared
                    .retained_artifacts
                    .withdraw_static_policy(artifact);
                if issued_restore {
                    return Err(crate::transcode::vod_refusal_error(
                        "retained_artifact_unavailable",
                        "the issued static output policy is obsolete",
                    ));
                }
                return Ok(None);
            }
        }
        Ok(selected)
    }

    /// Resolve native prerequisites without reserving a reader or attaching a
    /// rendition. Existing preparation owners may receive copy-index demand.
    pub(crate) async fn preview_recipe(
        &self,
        mut prepared: VodRecipeRequest<'_>,
        file: &MediaFile,
        settings: &VodSettings,
        viewer: Option<&crate::state::PlaybackViewerDemand>,
    ) -> Result<Option<Arc<crate::vodencode::Encoding>>, String> {
        self.prepare_recipe(&mut prepared, file, settings, viewer)
            .await
            .map(|prepared| prepared.recipe.encoding)
    }

    async fn prepare_recipe(
        &self,
        prepared: &mut VodRecipeRequest<'_>,
        file: &MediaFile,
        settings: &VodSettings,
        viewer: Option<&crate::state::PlaybackViewerDemand>,
    ) -> Result<PreparedVodRecipe, String> {
        let req = prepared.request;
        let (aac, preserve_dolby_vision, convert_dolby_vision) = match req.kind {
            SessionKind::Copy {
                aac,
                preserve_dolby_vision,
                convert_dolby_vision,
                ..
            } if prepared.encoding.is_none() => (aac, preserve_dolby_vision, convert_dolby_vision),
            _ if prepared.encoding.is_some() => (
                true,
                prepared
                    .encoding
                    .as_ref()
                    .is_some_and(|encoding| encoding.preserves_processed_dv()),
                false,
            ),
            _ => {
                return Err(crate::transcode::vod_refusal_error(
                    "vod_recipe_unresolved",
                    "the encoded VOD request has no resolved encoder recipe",
                ))
            }
        };
        if req.subtitle_burn.is_some() && prepared.encoding.is_none() {
            return Err(crate::transcode::vod_refusal_error(
                "vod_recipe_unresolved",
                "a subtitle burn requires a resolved encoded VOD recipe",
            ));
        }
        // A NULL/unprobed duration cannot be described by a closed film-time
        // playlist. Refuse it honestly; the removed live presentation is not
        // a substitute.
        let Some(duration_ms) = file.duration_ms.filter(|ms| *ms > 0) else {
            return Err(crate::transcode::vod_refusal_error(
                "vod_source_unsupported",
                "the file has no probed duration, so no immutable plan can be built",
            ));
        };
        let phase_started = Instant::now();
        // The selected ordinary AVC path has no Dolby Vision work. Avoid an
        // unrelated first-use FFmpeg probe while an admitted fixed worker is
        // planning or reattaching its exact retained rendition.
        let have_dovi = if prepared.encoding.is_some() && file.dolby_vision.is_empty() {
            false
        } else {
            crate::ffmpeg::has_dovi_rpu().await
        };
        tracing::debug!(target: "plurxd::vodserve", file_id = file.id,
            phase = "pipeline_capability", elapsed_ms = phase_started.elapsed().as_millis(),
            "rendition preparation phase completed");
        // An encoded rendition never carries the source's parameter sets, so
        // only a copy waits on the census.
        let probe_json = if prepared.encoding.is_none() {
            crate::hevc_census::probe_json_for_copy(self.shared.store.as_ref(), file).await
        } else {
            self.shared.store.get_file_probe_json(file.id).await
        }
        .map_err(|error| format!("reading the file probe: {error}"))?;
        let video = copy_video_pipeline(
            file,
            probe_json.as_deref(),
            have_dovi,
            preserve_dolby_vision,
            convert_dolby_vision,
        );
        let identity = match prepared.encoding.as_ref() {
            Some(encoding) => encoding.identity(file, duration_ms as f64 / 1_000.0),
            None => crate::fragindex::identity_for(file, video),
        };
        let cluster_cache_enabled = prepared.encoding.is_none() && settings.index_cluster_cache;
        let cluster_index = if prepared.encoding.is_some() {
            Ok(None)
        } else if cluster_cache_enabled {
            self.try_cluster_fragment_index(file, video, viewer).await
        } else {
            Ok(None)
        };
        let needs_attestation = cluster_cache_enabled && matches!(&cluster_index, Ok(None));
        let unavailable_reason = cluster_index.as_ref().err().cloned();
        let (index, source_object_version, cluster_cache_key) = match cluster_index {
            Ok(Some((index, object_version, cache_key))) => {
                (Some(index), Some(object_version), Some(cache_key))
            }
            Ok(None) if prepared.encoding.is_some() => (
                None,
                prepared
                    .encoding
                    .as_ref()
                    .map(|encoding| encoding.source_object_version.clone()),
                None,
            ),
            Ok(None) => (
                self.shared
                    .store
                    .fragment_index(file.id, &identity)
                    .await
                    .map_err(|error| format!("reading the fragment index: {error}"))?,
                None,
                None,
            ),
            Err(reason) => {
                // The first rollout phase is write/shadow plus prefer-v2.
                // Per-key fallback preserves an already healthy v1 title
                // until this exact source/pipeline key is fully available.
                tracing::debug!(target: "plurxd::vodserve", file_id = file.id, %reason, "v2 fragment index unavailable; using v1");
                (
                    self.shared
                        .store
                        .fragment_index(file.id, &identity)
                        .await
                        .map_err(|error| format!("reading the fragment index: {error}"))?,
                    None,
                    None,
                )
            }
        };
        // HEVC safety is established on original headers, before filters can
        // hide updates. Bind the proof to this node's current source object;
        // a peer's filesystem identity is not a local attestation.
        // A copy that keeps its in-band parameter sets cannot decode against
        // stale definitions, so it needs no proof that deleting them is safe.
        let source_object_version = if prepared.encoding.is_none()
            && matches!(file.video_codec.as_deref(), Some("hevc" | "h265"))
            && !video.retains_hevc_parameter_sets()
            && !settings.hevc_unverified_copy
        {
            let current = crate::fragment_index_cluster::inspect_source(file)
                .await
                .map_err(|reason| {
                    crate::transcode::vod_refusal_error("hevc_configuration_unverified", reason)
                })?;
            let proof = index
                .as_ref()
                .and_then(|index| index.promotion.hevc_configuration.as_ref());
            if !proof.is_some_and(|proof| {
                proof.permits_on_node(
                    &current,
                    self.shared.cluster_node_id.as_deref().unwrap_or_default(),
                )
            }) {
                let reason = proof
                    .filter(|proof| {
                        proof.source_object_version == current
                            && Some(proof.source_node_id.as_str())
                                == self.shared.cluster_node_id.as_deref()
                    })
                    .and_then(|proof| proof.refusal.as_deref());
                let mut preparation = if cluster_cache_enabled {
                    "HEVC copy needs preparation".to_owned()
                } else {
                    "HEVC copy needs preparation; shared preparation is disabled".to_owned()
                };
                if reason.is_none() && cluster_cache_enabled {
                    if let Some(node) = self.shared.cluster_node_id.as_deref() {
                        preparation =
                            match crate::state::enqueue_copy_preparation_for_object_with_viewer(
                                self.shared.store.as_ref(),
                                node,
                                file,
                                video,
                                Some(&current),
                                viewer,
                            )
                            .await
                            {
                                Ok(request) => {
                                    format!("HEVC exact copy preparation is {}", request.state)
                                }
                                Err(error) => {
                                    format!("HEVC copy preparation could not be queued: {error}")
                                }
                            };
                    }
                }
                let detail = reason.map(str::to_owned).unwrap_or_else(|| {
                    format!(
                    "{preparation}; Settings → Developer can enable unverified copy without waiting"
                )
                });
                return Err(crate::transcode::vod_refusal_error(
                    if reason.is_some() {
                        "hevc_configuration_unsupported"
                    } else {
                        "hevc_configuration_unverified"
                    },
                    detail,
                ));
            }
            Some(current)
        } else {
            source_object_version
        };
        if index.is_none() && prepared.encoding.is_none() {
            let reason = if needs_attestation {
                match self.shared.cluster_node_id.as_deref() {
                    Some(node_id) => {
                        match crate::state::enqueue_copy_preparation_for_object_with_viewer(
                            self.shared.store.as_ref(),
                            node_id,
                            file,
                            video,
                            None,
                            viewer,
                        )
                        .await
                        {
                            Ok(request) => format!(
                                "exact copy preparation is {}{}",
                                request.state,
                                if request.last_error_code.is_empty() {
                                    String::new()
                                } else {
                                    format!(": {}", request.last_error_code)
                                }
                            ),
                            Err(error) => {
                                format!("exact copy preparation could not be queued: {error}")
                            }
                        }
                    }
                    None => "this process has no cluster index identity".to_owned(),
                }
            } else {
                // The node-local pass builds this index; what its last
                // attempt found decides the refusal (`local_index_refusal`).
                let (code, reason) = self
                    .shared
                    .local_index_refusal(
                        file.id,
                        &identity,
                        unavailable_reason.unwrap_or_else(|| {
                            "shared preparation is disabled; no matching local index exists"
                                .to_owned()
                        }),
                    )
                    .await;
                return Err(crate::transcode::vod_refusal_error(code, reason));
            };
            // This is a prerequisite refusal, not a claim that a worker is
            // active. The durable analysis row and reason carry its actual
            // queued/running/failed state; the caller keeps rolling first play.
            return Err(crate::transcode::vod_refusal_error(
                "vod_index_pending",
                reason,
            ));
        }
        // The §2 ruling: a single immutable init cannot describe a film whose
        // clean fragments carry varying parameter sets, so the verdict is a
        // scan-time fallback here, never a producer_failed mid-playback.
        if video.retains_hevc_parameter_sets()
            && index
                .as_ref()
                .is_some_and(|index| !index.promotion.dvh1_parameter_sets.is_empty())
        {
            return Err(crate::transcode::vod_refusal_error(
                "vod_source_unsupported",
                "dvh1 completeness repair cannot retain in-band parameter sets",
            ));
        }
        if index
            .as_ref()
            .is_some_and(|index| !index.parameter_sets_constant)
        {
            return Err(crate::transcode::vod_refusal_error(
                "vod_source_unsupported",
                "its parameter sets vary mid-film (the §2 ruling)",
            ));
        }
        // Sharing may return an existing rendition and discard this Recipe.
        // Issued proof compatibility belongs to this incoming resolved request.
        let incoming_logical = Some(crate::vodserve::retained_manifest::LogicalOutput::resolve(
            req,
            prepared.encoding.as_deref(),
            file,
            video,
        ));
        let recipe = Recipe {
            retained_logical: incoming_logical.clone(),
            measured_candidate: prepared.measured_candidate.take(),
            file: file.clone(),
            audio_index: req.audio_index,
            aac,
            audio_delivery: prepared
                .encoding
                .as_ref()
                .and_then(|encoding| encoding.options.audio.clone())
                .or_else(|| req.audio_delivery.clone()),
            video,
            source_object_version,
            cluster_cache_key,
            encoding: prepared.encoding.take(),
        };
        Ok(PreparedVodRecipe {
            identity,
            index,
            recipe,
            duration_ms,
            incoming_logical,
        })
    }

    pub(super) async fn reserve_media_group(
        media: &[Arc<Rendition>],
        priority: Option<crate::admission::Priority>,
    ) -> Result<Vec<crate::vodencode::EncodePermit>, String> {
        let refuse = || {
            crate::transcode::vod_refusal_error(
                "vod_family_capacity",
                "the continuous media group cannot be admitted together",
            )
        };
        // Both create and controlled renewal supply sorted unique recipe
        // keys. Enforce that order before acquiring any binding: overlapping
        // groups always lock A before B, and a duplicate cannot self-deadlock.
        if media.is_empty()
            || media.len() > 3
            || media.windows(2).any(|pair| pair[0].key >= pair[1].key)
        {
            return Err(refuse());
        }
        let encodings = media
            .iter()
            .map(|rendition| rendition.recipe.encoding.as_ref().ok_or_else(refuse))
            .collect::<Result<Vec<_>, _>>()?;
        let first = encodings.first().ok_or_else(refuse)?;
        if encodings
            .iter()
            .any(|encoding| !first.admissions.shares_pool(&encoding.admissions))
        {
            return Err(refuse());
        }
        let Ok(Ok((hardware, software))) = tokio::time::timeout(
            Duration::from_secs(1),
            plurx_core::resource_settings::admission_pair(first.store.as_ref()),
        )
        .await
        else {
            return Err(refuse());
        };
        let hardware_limit = hardware
            .and_then(|value| value.trim().parse().ok())
            .unwrap_or(crate::admission::DEFAULT_MAX_HW_SESSIONS);
        let software_budget = software
            .and_then(|value| value.trim().parse().ok())
            .unwrap_or_else(crate::admission::software_budget);
        let estimates = encodings
            .iter()
            .map(|encoding| encoding.resources())
            .collect::<Vec<_>>();
        let priority = priority.unwrap_or_else(|| {
            if encodings
                .iter()
                .any(|encoding| encoding.is_speculative() || encoding.nonpreemptive_trial)
            {
                crate::admission::Priority::Speculative
            } else {
                crate::admission::Priority::Live
            }
        });
        // Foreground family starts participate in the same preemption protocol
        // as ordinary encoders. Keep the waiter alive across every attempt so
        // background indexing yields and cannot reclaim its released budget.
        // Speculative renewals never preempt an active viewer or wait here.
        let mut waiting = (priority == crate::admission::Priority::Live)
            .then(|| first.admissions.queue_for_slot());
        let deadline = Instant::now() + crate::admission::QUEUE_WAIT;
        loop {
            if let Some(permits) = Self::try_reserve_media_group(
                media,
                &first.admissions,
                hardware_limit,
                software_budget,
                &estimates,
                priority,
            ) {
                return Ok(permits);
            }
            if let Some(waiting) = waiting.as_mut() {
                waiting.mark_capacity_blocked();
            }
            let now = Instant::now();
            if priority != crate::admission::Priority::Live || now >= deadline {
                return Err(refuse());
            }
            tokio::time::sleep(crate::transcode::ADMISSION_POLL.min(deadline - now)).await;
        }
    }

    // One atomic attempt. Binding guards must never survive an await: workers
    // need those bindings to release capacity while a foreground start waits.
    fn try_reserve_media_group(
        media: &[Arc<Rendition>],
        admissions: &crate::admission::Admissions,
        hardware_limit: usize,
        software_budget: usize,
        estimates: &[crate::admission::TranscodeResourceEstimate],
        priority: crate::admission::Priority,
    ) -> Option<Vec<crate::vodencode::EncodePermit>> {
        // The callers hold these renditions' sorted build gates. Stabilize
        // weak bindings as well: an ordinary worker must not substitute a
        // newly acquired, unrelated credit after the pool decision adopts
        // the existing roles. There are no awaits under these short locks.
        let mut bindings = media
            .iter()
            .map(|rendition| rendition.retained_admission.lock_binding())
            .collect::<Vec<_>>();
        let mut retained = bindings
            .iter()
            .map(|binding| binding.current())
            .collect::<Vec<_>>();
        let views = retained
            .iter()
            .map(|permit| {
                permit
                    .as_ref()
                    .map(crate::vodencode::EncodePermit::retained_resources)
            })
            .collect::<Vec<_>>();
        // A selected finite compiler can cap encoder creation above the old
        // pipeline estimate. Adopt its SAME larger original entitlement, never
        // silently substitute a smaller old credit or charge another permit.
        // Keep the existing pool/family/native-shape validation authoritative.
        if estimates.len() > 3 || estimates.len() != views.len() || media.len() != views.len() {
            return None;
        }
        let mut actual = [crate::admission::TranscodeResourceEstimate {
            hardware_slot: false,
            cpu_threads: 0,
            decoder_threads: None,
        }; 3];
        for (index, ((estimate, existing), rendition)) in
            estimates.iter().zip(&views).zip(media).enumerate()
        {
            actual[index] = *estimate;
            if rendition.dir.is_finite() {
                if let Some(existing) = existing {
                    let held = existing
                        .software
                        .map_or(0, crate::admission::SwPermit::threads);
                    if existing.hardware.is_some() != estimate.hardware_slot
                        || held < estimate.cpu_threads
                    {
                        return None;
                    }
                    actual[index].cpu_threads = held;
                }
            }
        }
        let (family, admitted) = admissions.try_admit_family(
            hardware_limit,
            software_budget,
            &actual[..estimates.len()],
            &views,
            priority,
        )?;
        let mut admitted = admitted.into_iter();
        for (binding, permit) in bindings.iter_mut().zip(&mut retained) {
            if permit.is_none() {
                *permit = Some(
                    binding.bind(admitted.next().expect("one permit per missing role").into()),
                );
            }
        }
        debug_assert!(
            retained.iter().all(|permit| {
                permit
                    .as_ref()
                    .and_then(crate::vodencode::EncodePermit::family_id)
                    == Some(family)
            }),
            "locked bindings preserve the atomically admitted family"
        );
        Some(
            retained
                .into_iter()
                .map(|permit| permit.expect("every role admitted"))
                .collect(),
        )
    }

    async fn try_create_with_release_fence(
        &self,
        mut prepared: VodRecipeRequest<'_>,
        file: &MediaFile,
        settings: &VodSettings,
        attribution: VodAttribution<'_>,
        session_id: String,
        fences: VodCreateFences<'_>,
    ) -> Result<VodStart, String> {
        if let Some(viewer) = &fences.viewer {
            viewer.require_local_authority().map_err(str::to_owned)?;
        }
        // The one funnel every create passes through: the plain entry point,
        // the cluster one that every shipped caller actually uses, and the
        // resurrection of a session from its durable route. Applying the
        // ceiling here rather than at construction is what lets an operator
        // change it without a restart — a cap set only in `WaitPool::new`
        // would be frozen at whatever the node booted with — and applying it
        // at `try_create` alone reached no production path at all.
        let req = prepared.request;
        // Resolve may discard the incoming recipe in favour of a healthy
        // cached rendition. Preserve this consumer's purpose before that.
        let nonpreemptive_trial = prepared
            .encoding
            .as_ref()
            .is_some_and(|encoding| encoding.nonpreemptive_trial);
        let admission_priority = if prepared
            .encoding
            .as_ref()
            .is_some_and(|encoding| encoding.is_speculative() || encoding.nonpreemptive_trial)
        {
            crate::admission::Priority::Speculative
        } else {
            crate::admission::Priority::Live
        };
        let passive_grant = if req.passive_vod {
            if !req.vod_only || req.presentation != crate::transcode::Presentation::Vod {
                return Err(crate::transcode::vod_refusal_error(
                    "vod_passive_policy_invalid",
                    "passive retention requires VOD-only service policy",
                ));
            }
            Some(
                self.shared
                    .passive_grants
                    .reserve(
                        &session_id,
                        attribution.supersession_user,
                        &req.playback_id,
                        req.request_id.as_deref().unwrap_or(""),
                        fences.release_fence.is_some(),
                    )
                    .map_err(|reason| {
                        crate::transcode::vod_refusal_error(
                            match reason {
                                passive_grant::Refusal::Capacity => "vod_passive_capacity",
                                passive_grant::Refusal::InvalidIdentity => {
                                    "vod_passive_policy_invalid"
                                }
                                passive_grant::Refusal::Unavailable => "vod_passive_route_expired",
                            },
                            "passive VOD route admission refused",
                        )
                    })?,
            )
        } else {
            None
        };
        let continuous = req
            .continuous_media
            .as_ref()
            .is_some_and(|media| media.role == crate::transcode::ContinuousMediaRole::Video);
        if prepared.root_admission.is_some() && continuous {
            return Err(crate::transcode::vod_refusal_error(
                "vod_family_invalid",
                "a root preparation cannot substitute for continuous family admission",
            ));
        }
        if prepared.soundtrack.is_some() && !continuous {
            return Err(crate::transcode::vod_refusal_error(
                "vod_family_invalid",
                "a shared soundtrack requires a continuous video parent",
            ));
        }
        if continuous && !file.audio_streams.is_empty() && prepared.soundtrack.is_none() {
            return Err(crate::transcode::vod_refusal_error(
                "vod_family_invalid",
                "the continuous video parent is missing its soundtrack",
            ));
        }
        if continuous {
            let video = prepared.encoding.as_ref().ok_or_else(|| {
                crate::transcode::vod_refusal_error(
                    "vod_family_invalid",
                    "continuous video has no executable recipe",
                )
            })?;
            if video.shared_audio.is_some()
                || video.plan.options().input_has_audio
                || video.options.video_sample_envelope
                    != plurx_core::transcode::VideoSampleEnvelope::ContinuousAvcHigh50
                || prepared.soundtrack.as_ref().is_some_and(|audio| {
                    audio.shared_audio.is_none()
                        || audio.source_object_version != video.source_object_version
                        || audio.grid != video.grid
                        || audio.options.audio_index != video.options.audio_index
                })
            {
                return Err(crate::transcode::vod_refusal_error(
                    "vod_family_invalid",
                    "continuous video and soundtrack recipes do not pair",
                ));
            }
        }
        if prepared.companion.is_some()
            != req
                .continuous_media
                .as_ref()
                .is_some_and(|media| media.autonomous_companion.is_some())
        {
            return Err(crate::transcode::vod_refusal_error(
                "vod_family_invalid",
                "the autonomous companion was not resolved",
            ));
        }
        if let Some((companion_request, companion)) = prepared.companion.as_ref() {
            let video = prepared.encoding.as_ref().ok_or_else(|| {
                crate::transcode::vod_refusal_error(
                    "vod_family_invalid",
                    "an autonomous companion requires an executable video parent",
                )
            })?;
            if !continuous
                || companion.shared_audio.is_some()
                || companion.plan.options().input_has_audio
                || companion.options.video_sample_envelope
                    != plurx_core::transcode::VideoSampleEnvelope::ContinuousAvcHigh50
                || companion.source_object_version != video.source_object_version
                || companion.grid != video.grid
                || companion.options.audio_index != video.options.audio_index
                || companion.plan.output_contract().effective_height()
                    == video.plan.output_contract().effective_height()
                || companion_request
                    .candidate_context
                    .as_ref()
                    .map(|context| context.candidate_id)
                    != req
                        .continuous_media
                        .as_ref()
                        .and_then(|media| media.autonomous_companion)
            {
                return Err(crate::transcode::vod_refusal_error(
                    "vod_family_invalid",
                    "the autonomous video recipes do not pair",
                ));
            }
        }
        let companion_rendition = match prepared.companion.as_ref() {
            Some((request, encoding)) => {
                let attachment = self
                    .resolve_rendition(
                        &mut VodRecipeRequest {
                            root_admission: None,
                            measured_candidate: None,
                            retained_capture: RetainedOutputCapture::New,
                            request,
                            encoding: Some(Arc::clone(encoding)),
                            soundtrack: None,
                            companion: None,
                        },
                        file,
                        settings,
                        fences.viewer.as_ref(),
                        None,
                        None,
                    )
                    .await?
                    .0;
                let rendition = Arc::clone(&attachment.rendition);
                drop(attachment);
                Some(rendition)
            }
            None => None,
        };
        // Prepare independently, then reacquire all build gates in key order.
        // No public reader exists until the exact cached objects are rechecked.
        let mut audio_request = req.clone();
        audio_request.candidate_context = None;
        if let Some(media) = audio_request.continuous_media.as_mut() {
            media.role = crate::transcode::ContinuousMediaRole::SharedAudio;
            media.autonomous_companion = None;
            media.companion_catalog = None;
            media.companion_context = None;
            media.family_descriptor = None;
        }
        let audio_rendition = match prepared.soundtrack.as_ref() {
            Some(soundtrack) => {
                let attachment = self
                    .resolve_rendition(
                        &mut VodRecipeRequest {
                            root_admission: None,
                            measured_candidate: None,
                            retained_capture: RetainedOutputCapture::New,
                            companion: None,
                            request: &audio_request,
                            encoding: Some(Arc::clone(soundtrack)),
                            soundtrack: None,
                        },
                        file,
                        settings,
                        fences.viewer.as_ref(),
                        None,
                        None,
                    )
                    .await?
                    .0;
                let rendition = Arc::clone(&attachment.rendition);
                drop(attachment);
                Some(rendition)
            }
            None => None,
        };
        let (attachment, incoming_logical) = self
            .resolve_rendition(
                &mut prepared,
                file,
                settings,
                fences.viewer.as_ref(),
                None,
                fences.expected_attempt,
            )
            .await?;
        let rendition = Arc::clone(&attachment.rendition);
        drop(attachment);
        let mut media = vec![Arc::clone(&rendition)];
        if let Some(audio) = audio_rendition.as_ref() {
            media.push(Arc::clone(audio));
        }
        if let Some(companion) = companion_rendition.as_ref() {
            media.push(Arc::clone(companion));
        }
        media.sort_unstable_by(|a, b| a.key.cmp(&b.key));
        if media.windows(2).any(|pair| pair[0].key == pair[1].key) {
            return Err(crate::transcode::vod_refusal_error(
                "vod_family_invalid",
                "autonomous media roles alias one immutable recipe",
            ));
        }
        let mut build_guards = Vec::with_capacity(media.len());
        for child in &media {
            build_guards.push(
                self.shared
                    .rendition_build_gate(&child.key)
                    .lock_owned()
                    .await,
            );
        }
        {
            let cached = self.shared.renditions.lock().await;
            if media.iter().any(|child| {
                child.closed.load(Acquire)
                    || !cached
                        .get(&child.key)
                        .is_some_and(|current| Arc::ptr_eq(current, child))
                    || child
                        .source
                        .as_ref()
                        .is_some_and(|source| !source.unchanged())
            }) {
                return Err(crate::transcode::vod_refusal_error(
                    "vod_source_rescan_required",
                    "continuous preparation changed before attachment",
                ));
            }
        }
        let mut private_media = if continuous {
            let permits = Self::reserve_media_group(&media, Some(admission_priority)).await?;
            media
                .into_iter()
                .zip(permits)
                .map(|(child, permit)| ParentMediaReader {
                    controlled: req
                        .continuous_media
                        .as_ref()
                        .is_some_and(|media| media.controlled)
                        && child
                            .recipe
                            .encoding
                            .as_ref()
                            .is_some_and(|encoding| encoding.shared_audio.is_none()),
                    candidate_id: if Arc::ptr_eq(&child, &rendition) {
                        req.candidate_context
                            .as_ref()
                            .map(|context| context.candidate_id)
                    } else if companion_rendition
                        .as_ref()
                        .is_some_and(|companion| Arc::ptr_eq(&child, companion))
                    {
                        req.continuous_media
                            .as_ref()
                            .and_then(|media| media.autonomous_companion)
                    } else {
                        None
                    },
                    reader_id: uuid::Uuid::new_v4().to_string(),
                    rendition: child,
                    _reservation: Some(permit),
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };

        // The sorted build gates still pin the exact cached rendition here.
        // Same-key substitution cannot spend another recipe's prepared credit.
        let processed_reuse_scope = prepared
            .root_admission
            .as_ref()
            .and_then(|root| root.reuse_scope);
        let attachment_owner = fences
            .release_fence
            .as_ref()
            .and_then(|fence| fence.attachment_owner);
        let mut reused_parent = None;
        let root_admission = prepared
            .root_admission
            .take()
            .map(|prepared| {
                if !rendition
                    .recipe
                    .encoding
                    .as_ref()
                    .is_some_and(|encoding| Arc::ptr_eq(encoding, &prepared.encoding))
                {
                    return Err(crate::transcode::vod_refusal_error(
                        "vod_preparation_changed",
                        "the admitted executable changed before attachment",
                    ));
                }
                if prepared.reused_parent.is_some()
                    && !rendition
                        .retained_admission
                        .current()
                        .is_some_and(|current| current.same_reservation(&prepared.permit))
                {
                    return Err(crate::transcode::vod_refusal_error(
                        "vod_preparation_changed",
                        "the captured processing reservation changed",
                    ));
                }
                reused_parent = prepared.reused_parent;
                Ok(rendition.retained_admission.bind(prepared.permit))
            })
            .transpose()?;

        let start_entry = entry_containing(&rendition.plan, req.start_seconds);
        let retained_output = if rendition.ephemeral_processing {
            None
        } else {
            self.capture_retained_output(
                prepared.retained_capture,
                &rendition,
                &incoming_logical,
                req,
                file,
                continuous,
            )
            .await?
        };
        let marker_destinations = stored_marker_destinations(
            self.shared.store.as_ref(),
            file,
            &rendition.plan,
            rendition.seconds_per_segment,
        )
        .await;
        if reused_parent.is_some() {
            let encoding = rendition
                .recipe
                .encoding
                .as_ref()
                .ok_or("processed executable missing")?;
            if !encoding
                .engine
                .is_current_with_executable(&encoding.executable)
                .await
            {
                return Err(crate::transcode::vod_refusal_error(
                    "vod_preparation_changed",
                    "processed executable changed during attachment",
                ));
            }
        }
        let _release_transition = if let Some(release_fence) = fences.release_fence {
            let guard = release_fence.transition.lock_owned().await;
            if release_fence.released.load(Acquire) {
                return Err(crate::transcode::vod_refusal_error(
                    "vod_session_released",
                    "the VOD session was released before attachment",
                ));
            }
            Some(guard)
        } else {
            None
        };
        let lifecycle = self.shared.session_lifecycle(&session_id);
        let lifecycle_guard = Arc::clone(&lifecycle).lock_owned().await;
        let duration_ms = plan_duration_ms(&rendition.plan);
        if retained_output.as_ref().is_some_and(|artifact| {
            self.shared
                .retained_artifacts
                .acquire_expected_for_request(&artifact.facts(), &rendition, &incoming_logical)
                .is_none()
        }) {
            return Err(crate::transcode::vod_refusal_error(
                "retained_artifact_unavailable",
                "the exact artifact changed before attachment",
            ));
        }
        let mut replacement = Session {
            children: Vec::new(),
            root_admission,
            processed_reuse_scope,
            processed_reuse_seed: processed_reuse_scope
                .and_then(|_| ProcessedReuseSeed::from_request(req)),
            preferred_start_ms: super::vod_serve_delivery::preferred_playlist_start_ms(
                req.start_seconds,
                duration_ms,
            ),
            passive_grant: passive_grant.clone(),
            retained_output,
            rendition: Some(Arc::clone(&rendition)),
            rendition_key: rendition.key.clone(),
            file: Arc::new(rendition.recipe.file.clone()),
            playback_id: req.playback_id.clone(),
            user_name: attribution.user_name.to_owned(),
            item_title: attribution.item_title.to_owned(),
            started_unix: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_secs().min(i64::MAX as u64) as i64)
                .unwrap_or(0),
            target_height: rendition
                .recipe
                .encoding
                .as_ref()
                .map_or(file.height.unwrap_or(0), |encoding| {
                    encoding.options.target_height
                }),
            kind: rendition
                .recipe
                .encoding
                .as_ref()
                .map_or(req.kind, |encoding| SessionKind::Transcode {
                    height: encoding.options.target_height,
                }),
            supersession_user: attribution.supersession_user.to_owned(),
            block_budget: settings.block_budget,
            sdr_master_codecs: settings.sdr_master_codecs,
            lifecycle: Arc::clone(&lifecycle),
            incarnation: Arc::new(()),
            last_touch: StdMutex::new(Instant::now()),
            // The same decision as `kind` above: an encoded rendition is a
            // transcode whatever the request asked for.
            delivery: Arc::new(crate::meter::Meter::for_method(
                if rendition.recipe.encoding.is_some()
                    || matches!(req.kind, SessionKind::Transcode { .. })
                {
                    "transcode"
                } else {
                    "remux"
                },
            )),
            control: StdMutex::new(crate::playback_control::ControlState::default()),
            marker_destinations,
            control_observed_at: None,
            last_control_snapshot: None,
            control_end: None,
            control_end_snapshot: None,
            prepared_incarnation: None,
            #[cfg(target_os = "linux")]
            finite_session_slot: None,
            #[cfg(target_os = "linux")]
            finite_control_slot: None,
            terminal_cleanup: None,
            tombstone: None,
        };

        #[cfg(target_os = "linux")]
        replacement.reserve_finite_session_state(&session_id)?;

        // Acquire every async lock before changing either the reader graph or
        // the registry. Cancellation while resurrecting is therefore a clean
        // no-op; after the final lock is acquired the attachment replacement
        // is one synchronous transaction with no partial externally visible
        // state.
        let mut sessions = self.shared.sessions.lock().await;
        if sessions
            .get(&session_id)
            .is_some_and(|session| session.tombstone.is_some())
        {
            return Err(crate::transcode::vod_refusal_error(
                "vod_session_ended",
                "the VOD session already has a terminal tombstone",
            ));
        }
        let previous_rendition = sessions
            .get(&session_id)
            .and_then(|session| session.rendition.as_ref().map(Arc::clone));
        let mut previous_readers = match previous_rendition.as_ref() {
            Some(previous) if !Arc::ptr_eq(previous, &rendition) => {
                Some(previous.readers.lock().await)
            }
            _ => None,
        };
        // Set only where this attachment actually moves the viewer off another
        // rendition. Stopping the flight has to happen after the guards below
        // are dropped — settlement waits on a real ffmpeg, and holding a
        // rendition-wide lock across that would let one viewer's recipe change
        // stall every other reader of the same rendition — so what crosses the
        // drop is the flight's identity, not the decision to stop it.
        let mut obsolete_window_flight: Option<u64> = None;
        let mut audio_readers = match audio_rendition.as_ref() {
            Some(audio)
                if !previous_rendition
                    .as_ref()
                    .is_some_and(|previous| Arc::ptr_eq(previous, audio)) =>
            {
                Some(audio.readers.lock().await)
            }
            _ => None,
        };
        let mut companion_readers = match companion_rendition.as_ref() {
            Some(companion)
                if !previous_rendition
                    .as_ref()
                    .is_some_and(|previous| Arc::ptr_eq(previous, companion)) =>
            {
                Some(companion.readers.lock().await)
            }
            _ => None,
        };
        let mut replacement_readers = rendition.readers.lock().await;
        let _serving_transition = if let Some(admission) = fences.serving_admission.as_ref() {
            Some(admission.commit_guard_before().await.ok_or_else(|| {
                crate::transcode::serving_fence_error(crate::serving_fence::SERVING_FENCED_MESSAGE)
            })?)
        } else {
            None
        };
        if passive_grant.as_ref().is_some_and(|grant| !grant.live()) {
            return Err(crate::transcode::vod_refusal_error(
                "vod_passive_route_expired",
                "passive route expired during preparation",
            ));
        }
        if let Some(reuse) = reused_parent.as_ref() {
            let valid = sessions.get(&reuse.session_id).is_some_and(|previous| {
                replacement
                    .root_admission
                    .as_ref()
                    .is_some_and(|permit| reuse.matches(previous, permit))
            });
            if !valid {
                return Err(crate::transcode::vod_refusal_error(
                    "vod_preparation_changed",
                    "the exact processing predecessor ended or changed",
                ));
            }
        }
        // The selected finite recipe has no private child rendition. Refuse
        // any future expansion before changing the predecessor graph; each
        // private child still uses the legacy map until it has its own finite
        // accounting contract.
        #[cfg(target_os = "linux")]
        if private_media
            .iter()
            .any(|child| child.rendition.finite_client_lease.is_some())
        {
            return Err("finite private child has no client-state attachment contract".into());
        }
        if !replacement_readers.has_room_for(&session_id) {
            return Err("finite reader table has no remaining slot".into());
        }
        let prepared_authority =
            rendition.prepare_client_reader(&session_id, start_entry, admission_priority)?;
        if let Some(previous_readers) = previous_readers.as_mut() {
            previous_readers.remove(&session_id);
            if previous_readers.is_empty() {
                if let Some(previous) = previous_rendition.as_ref() {
                    *previous.dormant_since.lock().expect("dormant lock") = Some(Instant::now());
                }
            }
            // Reattachment removes the reader here rather than through
            // `detach_reader`, so it needs the same wait retirement: this
            // viewer has moved to another rendition, and any request still
            // parked on the one they left has no reader to be ranked against.
            // `playback_demands` would mark it foreground on exactly that
            // absence and keep the old rendition producing for somebody who
            // is no longer watching it.
            if let Some(previous) = previous_rendition.as_ref() {
                self.shared.pool.retire_session(&previous.key, &session_id);
            }
            // The third thing `detach_reader` does, which this path was
            // missing, and which it cannot do the same way. A subtitle window
            // is keyed by session id alone, so the flight this viewer left
            // behind is a live ffmpeg extracting a span for the recipe they
            // just moved off — worth stopping, and `detach_reader`'s comment
            // claims every ending converges here to stop it.
            //
            // It cannot call `release_session_window`, because releasing also
            // fences the id for half a minute and this id belongs to a viewer
            // who is still watching: the fence would refuse the first window
            // of the attachment replacing it and turn a recipe change into
            // half a minute without subtitles. It names the exact flight
            // instead, read here under the same guard that decides it is
            // obsolete, so a successor claiming the session between this read
            // and the stop is left alone.
            obsolete_window_flight = crate::subtitles::session_window_flight(&session_id);
        }
        rendition.revoke_preparation();
        for child in &private_media {
            let entry = media_entry_containing_ms(
                &child.rendition.plan,
                (req.start_seconds.max(0.0) * 1000.0) as i64,
            );
            if Arc::ptr_eq(&child.rendition, &rendition) {
                replacement_readers
                    .try_insert(child.reader_id.clone(), {
                        let mut reader = Reader::with_priority(entry, admission_priority);
                        reader.nonpreemptive_trial = nonpreemptive_trial;
                        reader
                    })
                    .expect("private-media reader uses the legacy table");
            } else {
                let readers = if companion_rendition
                    .as_ref()
                    .is_some_and(|companion| Arc::ptr_eq(companion, &child.rendition))
                {
                    companion_readers.as_mut().or(previous_readers.as_mut())
                } else {
                    audio_readers.as_mut().or(previous_readers.as_mut())
                }
                .expect("private media reader guard");
                readers
                    .try_insert(child.reader_id.clone(), {
                        let mut reader = Reader::with_priority(entry, admission_priority);
                        reader.nonpreemptive_trial = nonpreemptive_trial;
                        reader
                    })
                    .expect("private-media reader uses the legacy table");
                *child.rendition.dormant_since.lock().expect("dormant lock") = None;
            }
        }
        replacement.children = std::mem::take(&mut private_media);
        let mut authority = prepared_authority.1;
        authority.authority_only = replacement.retained_output.is_some()
            || replacement.children.iter().any(|child| child.controlled);
        authority.admission_priority = admission_priority;
        authority.nonpreemptive_trial = nonpreemptive_trial;
        replacement_readers
            .try_insert(prepared_authority.0, authority)
            .expect("preflighted authority insertion remains within guarded table");
        *rendition.dormant_since.lock().expect("dormant lock") = None;
        // A live entry with this id is replaced rather than refused, and the
        // replacement carries a fresh `ControlState` — so a staged M6
        // successor belonging to the outgoing attachment would simply vanish
        // while its durable row lived on. Abort it first, and note that the
        // replacement mints a new `incarnation`: a gate held for the outgoing
        // attachment is pinned to the old one and refuses everything from
        // here, rather than taking the newcomer's slot.
        if let Some(outgoing) = sessions.get(&session_id) {
            outgoing.abort_staged_preparation();
        }
        let outgoing_children = sessions
            .get_mut(&session_id)
            .map(|session| std::mem::take(&mut session.children))
            .unwrap_or_default();
        if let Some(sink) = attachment_owner {
            *sink.lock().expect("first preparation owner") = Some(replacement.response_owner());
        }
        sessions.insert(session_id.clone(), replacement);
        {
            let mut observation = self
                .shared
                .preparation_attachment
                .lock()
                .expect("attachment observation");
            *observation = observation.saturating_add(1);
        }
        // Both reader graphs are committed, so the rendition-wide guards have
        // no further work. They used to live to the end of the function, which
        // was free while nothing here awaited; the window stop below does, and
        // settlement waits on a real ffmpeg. Holding either guard across that
        // would let one viewer's recipe change stall every other reader of the
        // rendition they left or the one they joined.
        drop(replacement_readers);
        drop(audio_readers);
        drop(companion_readers);
        drop(previous_readers);
        drop(_serving_transition);
        drop(sessions);
        let cleanup_id = session_id.clone();
        let shared = Arc::clone(&self.shared);
        let file_id = file.id;
        let file_height = file.height.unwrap_or(0);
        let kind = req.kind;
        // Wake the committed reader graph before any asynchronous cleanup.
        // Cancellation cannot strand an attached parent without a driver kick.
        rendition.kick();
        if let Some(audio) = audio_rendition.as_ref() {
            audio.kick();
        }
        if let Some(companion) = companion_rendition.as_ref() {
            companion.kick();
        }
        if outgoing_children.is_empty() && obsolete_window_flight.is_none() {
            self.emit_lifecycle(
                &cleanup_id,
                file_id,
                file_height,
                kind,
                "session_start",
                None,
            );
            drop(lifecycle_guard);
        } else {
            let cleanup = spawn_cancellation_independent(async move {
                let _lifecycle = lifecycle_guard;
                // Child identities belong to the outgoing incarnation. Retiring
                // them cannot fence parent captions or remove a new private reader.
                for child in outgoing_children {
                    child.detach(&shared.pool).await;
                }
                if let Some(flight) = obsolete_window_flight {
                    crate::subtitles::abandon_session_window(&cleanup_id, flight).await;
                }
                VodServe { shared }.emit_lifecycle(
                    &cleanup_id,
                    file_id,
                    file_height,
                    kind,
                    "session_start",
                    None,
                );
            });
            let _ = cleanup.await;
        }
        // The reader graph and registry are now one committed attachment.
        // Release does not wait for producer wakeup, tracing or lifecycle
        // event publication, and can tombstone this exact incarnation before
        // the caller attempts to acquire a response owner.
        drop(_release_transition);
        // Reader graph and registry now own the exact handle. Releasing the
        // per-key build gate before this point would let a dormant purge
        // remove it in the lookup/attach gap.
        drop(build_guards);
        tracing::info!(
            target: "plurxd::vodserve",
            session = %session_log_id(&session_id),
            rendition = %rendition.key,
            file = file.id,
            "vod session attached (start entry {start_entry})"
        );
        Ok(VodStart {
            session_id,
            duration_ms,
        })
    }
}

pub(super) struct PreparedVodRendition {
    pub(super) attachment: RenditionAttachment,
    start_entry: u32,
    marker_destinations: Vec<MarkerDestination>,
}

/// Private build result. Physical admission and fresh first activation still
/// precede any viewer attachment or driver demand. No wire constructor.
#[allow(dead_code)] // The owned Source actor is the only planned consumer.
pub(crate) struct PreparedSourceVodRendition {
    prepared: PreparedVodRendition,
    request: SessionRequest,
    file: MediaFile,
    settings: VodSettings,
    assignment: plurx_core::sharing_source_sessions::SourceDispatchAssignment,
}

/// A pending Source VOD rendition and its real physical reservation.
/// Dropping this before attachment returns only that reservation; it never
/// retires the immutable database assignment or certifies a spawned child.
#[allow(dead_code)] // The private Source actor consumes this first-start handoff.
pub(crate) struct AdmittedSourceVodRendition {
    pending: PreparedSourceVodRendition,
    permit: crate::vodencode::EncodePermit,
}

/// Actual admitted rendition facts before the actor's blocked activation.
/// This observation is neither producer readiness nor publication authority.
pub(crate) struct SourcePendingStartInfo {
    info: crate::transcode::StartInfo,
}
impl SourcePendingStartInfo {
    pub(crate) fn start_info(&self) -> &crate::transcode::StartInfo {
        &self.info
    }
}

pub(crate) struct ReservedSourceVodRendition {
    pending: Option<PreparedSourceVodRendition>,
    permit: Option<crate::vodencode::EncodePermit>,
    owner: Arc<source_lifetime::SourceRenditionOwner>,
    rendition: Arc<Rendition>,
    assignment: plurx_core::sharing_source_sessions::SourceDispatchAssignment,
    session_id: Option<String>,
}

pub(crate) struct SourceCopyReadiness {
    assignment: plurx_core::sharing_source_sessions::SourceDispatchAssignment,
    _registration: crate::prodrun::ProducerRegistration,
    _init_identity: String,
    _source: crate::fragment_index_cluster::SourceFence,
}
#[allow(dead_code)] // Consumed by the private owned Source actor, pending HTTP integration.
impl SourceCopyReadiness {
    pub(crate) fn matches(
        &self,
        assignment: &plurx_core::sharing_source_sessions::SourceDispatchAssignment,
    ) -> bool {
        self.assignment.same_identity(assignment) && self._source.unchanged()
    }
}

#[allow(dead_code)] // Consumed by the private owned Source actor, pending HTTP integration.
impl ReservedSourceVodRendition {
    pub(crate) fn start_info(&self, session_id: &str) -> Result<SourcePendingStartInfo, String> {
        let uuid = uuid::Uuid::parse_str(session_id)
            .map_err(|_| "invalid actual Source session UUID".to_owned())?;
        if uuid.get_version() != Some(uuid::Version::Random) || uuid.to_string() != session_id {
            return Err("invalid actual Source session UUID".into());
        }
        let pending = self
            .pending
            .as_ref()
            .ok_or_else(|| "Source attachment has already committed".to_owned())?;
        Ok(SourcePendingStartInfo {
            info: crate::transcode::StartInfo {
                processed_dv_profile: self
                    .rendition
                    .recipe
                    .encoding
                    .as_ref()
                    .filter(|encoding| encoding.preserves_processed_dv())
                    .map(|_| 8),
                processed_fel: self
                    .rendition
                    .recipe
                    .encoding
                    .as_ref()
                    .and_then(|encoding| encoding.processed_fel()),
                degraded_processing: None,
                processing_note: None,
                session_id: session_id.into(),
                playlist_url: format!("/api/v1/hls/{session_id}/index.m3u8"),
                duration_ms: Some(plan_duration_ms(&self.rendition.plan)),
                start_seconds: 0.0,
                media_origin_seconds: 0.0,
                target_height: self
                    .rendition
                    .recipe
                    .encoding
                    .as_ref()
                    .map_or(self.rendition.recipe.file.height.unwrap_or(0), |encoding| {
                        encoding.options.target_height
                    }),
                kind: pending.request.kind,
                encoder: self
                    .rendition
                    .recipe
                    .encoding
                    .as_ref()
                    .map_or("vod", |encoding| encoding.plan.encoder().label()),
                grade: self
                    .rendition
                    .recipe
                    .encoding
                    .as_ref()
                    .map_or(plurx_core::transcode::OutputGrade::Sdr, |encoding| {
                        encoding.delivered_output_grade()
                    }),
                vod: true,
                retained_output: None,
                audio_delivery: self.rendition.recipe.audio_delivery.clone(),
                control_lease_timeout_ms: crate::playback_control::VOD_LEASE_TIMEOUT_MS,
            },
        })
    }

    /// Wait for the registered producer's init, on the rendition's own signals.
    ///
    /// The init demand is registered the way an init GET registers it, so
    /// the driver has been kicked once and the materialize clock bounds the
    /// wait. Init landing, a recorded failure and that clock's expiry all wake
    /// `init_notify`; a producer registration wakes the owner when its
    /// dispatch settles. Both are armed before the checks, so a wake between
    /// a check and the wait is not lost. Every wake re-checks the physical
    /// fence; the init is re-read only when it may have changed, which is an
    /// identity this wait has not yet compared or a fresh init landing.
    pub(crate) async fn wait_ready(
        &self,
        serve: &VodServe,
        deadline: Instant,
    ) -> Result<SourceCopyReadiness, String> {
        let demand = serve
            .shared
            .arm_materialize_watchdog(&self.rendition, INIT_DEMAND_INDEX)
            .map_err(|error| format!("reserving init demand: {error}"))?;
        self.rendition.kick();
        let mut compared: Option<String> = None;
        loop {
            let landed = self.rendition.init_notify.notified();
            tokio::pin!(landed);
            landed.as_mut().enable();
            let registered = self.owner.dispatch_settled();
            tokio::pin!(registered);
            registered.as_mut().enable();
            if Instant::now() >= deadline {
                return Err("Source materialization timed out".into());
            }
            if let Some(cause) = self.rendition.failure_cause() {
                return Err(cause);
            }
            if self
                .rendition
                .source
                .as_ref()
                .is_none_or(|source| !source.unchanged())
            {
                return Err("Source physical identity changed before readiness".into());
            }
            if let Some(verdict) = demand.expired_verdict().await {
                return Err(verdict.message().to_owned());
            }
            if let Some(registration) = self.owner.registered_readiness() {
                let identity = self
                    .rendition
                    .identity
                    .lock()
                    .await
                    .identity
                    .as_ref()
                    .map(|identity| identity.served_init.clone())
                    .filter(|identity| compared.as_ref() != Some(identity));
                if let Some(identity) = identity {
                    if self.init_matches(&identity).await {
                        let held = self
                            .rendition
                            .source
                            .as_ref()
                            .ok_or_else(|| "Source physical fence is absent".to_owned())?;
                        let source = crate::fragment_index_cluster::open_source_playback_fence(
                            &self.rendition.recipe.file,
                            Some(held.object_version()),
                        )
                        .await?;
                        return Ok(SourceCopyReadiness {
                            assignment: self.assignment.clone(),
                            _registration: registration,
                            _init_identity: identity,
                            _source: source,
                        });
                    }
                    compared = Some(identity);
                }
            }
            let woke = tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), async {
                tokio::select! {
                    () = &mut landed => true,
                    () = &mut registered => false,
                }
            })
            .await;
            match woke {
                Err(_) => return Err("Source materialization timed out".into()),
                // New init bytes may carry an identity already compared.
                Ok(true) => compared = None,
                Ok(false) => {}
            }
        }
    }

    /// Whether the rendition's `init.mp4` on disk is exactly `identity`.
    async fn init_matches(&self, identity: &str) -> bool {
        let path = self.rendition.dir.path().join(INIT_NAME);
        match tokio::fs::metadata(&path).await {
            Ok(metadata)
                if metadata.is_file()
                    && metadata.len() > 0
                    && metadata.len() <= HEAD_REGENERATION_MAX_BYTES as u64 =>
            {
                tokio::fs::read(&path)
                    .await
                    .is_ok_and(|bytes| hex::encode(Sha256::digest(&bytes)) == identity)
            }
            _ => false,
        }
    }

    /// Resolves with the cause once this rendition records a failure.
    ///
    /// `record_failure` publishes the cause and then wakes `init_notify`; init
    /// landing and init-demand expiry wake it too, so each wake re-checks.
    pub(crate) async fn failed(&self) -> String {
        loop {
            let notified = self.rendition.init_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(cause) = self.rendition.failure_cause() {
                return cause;
            }
            notified.await;
        }
    }

    pub(crate) async fn retire(
        &mut self,
        serve: &VodServe,
    ) -> Result<source_lifetime::SourceProducerAssociationsSettled, String> {
        if let Some(session_id) = self.session_id.as_deref() {
            serve.end(session_id, Terminal::Replaced).await;
        }
        self.owner.begin_detach();
        self.owner.wait_dispatches().await;
        if let Some(registration) = self.rendition.source_owners.terminal_registration() {
            self.rendition
                .slot
                .request_registered_retirement(&registration)
                .await
                .map_err(|_| "Source producer retirement is unresolved".to_owned())?;
        }
        Ok(self.owner.detach_and_wait().await)
    }
}

#[cfg(test)]
impl AdmittedSourceVodRendition {
    pub(crate) async fn assert_no_demand_or_child(&self) {
        let rendition = &self.pending.prepared.attachment.rendition;
        assert!(rendition.readers.lock().await.is_empty());
        assert_eq!(rendition.last_child_pid.load(Relaxed), 0);
        assert!(matches!(
            rendition.producer_belief().await,
            Producer::Absent { .. }
        ));
    }
}

impl VodServe {
    #[allow(dead_code)] // The private owned Source actor is the sole consumer.
    pub(crate) fn reserve_source_vod(
        &self,
        admitted: AdmittedSourceVodRendition,
    ) -> Result<ReservedSourceVodRendition, String> {
        let rendition = Arc::clone(&admitted.pending.prepared.attachment.rendition);
        let assignment = admitted.pending.assignment.clone();
        let owner = rendition
            .source_owners
            .attach(&assignment)
            .map_err(str::to_owned)?;
        Ok(ReservedSourceVodRendition {
            pending: Some(admitted.pending),
            permit: Some(admitted.permit),
            owner,
            rendition,
            assignment,
            session_id: None,
        })
    }

    #[allow(dead_code)] // The private owned Source actor is the sole consumer.
    pub(crate) async fn commit_source_vod(
        &self,
        reserved: &mut ReservedSourceVodRendition,
        session_id: &str,
        producer_gate: &Arc<crate::transcode::source_actor::SourceProducerAuthority>,
        admissions: &crate::admission::Admissions,
    ) -> Result<VodStart, String> {
        let permit = reserved
            .permit
            .take()
            .ok_or_else(|| "Source physical reservation has already transferred".to_owned())?;
        if let Err(permit) = reserved.rendition.source_owners.arm_initial_permit(
            &reserved.owner,
            permit,
            admissions,
            producer_gate,
        ) {
            reserved.permit = Some(permit);
            return Err("Source first-start reservation is busy".into());
        }
        let pending = reserved
            .pending
            .take()
            .ok_or_else(|| "Source attachment has already committed".to_owned())?;
        reserved.session_id = Some(session_id.into());
        let owner_key = pending.assignment.binding().principal().owner_key();
        Box::pin(self.commit_source_rendition(
            &pending.request,
            &pending.file,
            &pending.settings,
            VodAttribution {
                user_name: "Shared viewer",
                item_title: "Shared playback",
                supersession_user: &owner_key,
            },
            session_id.into(),
            VodCreateFences {
                release_fence: None,
                serving_admission: None,
                expected_attempt: None,
                viewer: Some(crate::state::PlaybackViewerDemand {
                    principal: pending.assignment.binding().principal().clone(),
                    playback_id: pending.request.playback_id.clone(),
                }),
            },
            pending.prepared,
        ))
        .await
    }

    /// No codec/FFprobe/burn work is hidden in this copy-only preparation.
    /// The real reservation is held before first durable blocked activation.
    #[allow(dead_code)] // Source actor integration is deliberately incremental.
    pub(crate) async fn prepare_admitted_source_copy(
        &self,
        source: &crate::http::hls::PreparedSourcePlayback,
        assignment: &plurx_core::sharing_source_sessions::SourceDispatchAssignment,
        settings: &VodSettings,
        admissions: &crate::admission::Admissions,
        store: &dyn plurx_core::store::Store,
        deadline: Instant,
    ) -> Result<AdmittedSourceVodRendition, String> {
        Box::pin(self.prepare_admitted_source_vod(
            source, assignment, settings, admissions, store, deadline, None,
        ))
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn prepare_admitted_source_vod(
        &self,
        source: &crate::http::hls::PreparedSourcePlayback,
        assignment: &plurx_core::sharing_source_sessions::SourceDispatchAssignment,
        settings: &VodSettings,
        admissions: &crate::admission::Admissions,
        store: &dyn plurx_core::store::Store,
        deadline: Instant,
        encoding: Option<Arc<crate::vodencode::Encoding>>,
    ) -> Result<AdmittedSourceVodRendition, String> {
        // A burn is always encoded; its sidecar is owned by the encoding the
        // Source preparation built, never looked up here.
        if !source.matches_assignment(assignment)
            || (encoding.is_some()
                != crate::transcode::source_actor::source_recipe_is_encoded(source.request()))
            || encoding.as_ref().is_some_and(|encoding| {
                source.request().subtitle_burn.is_some() != encoding.subtitle.is_some()
            })
        {
            return Err("Source VOD preparation requires its exact recipe assignment".into());
        }
        // One live wait, bounded by the start deadline and the foreground
        // queue wait together. Either way the waiter stays registered across
        // attempts: the copy wait holds its own guard, and an encoding keeps
        // the waiter `try_permit` registered until it is granted.
        let admission_deadline = deadline.min(Instant::now() + crate::admission::QUEUE_WAIT);
        let admission =
            tokio::time::timeout_at(tokio::time::Instant::from_std(admission_deadline), async {
                let Some(encoding) = &encoding else {
                    return crate::vodencode::EncodePermit::admit_source_copy(
                        admissions,
                        store,
                        admission_deadline,
                    )
                    .await;
                };
                loop {
                    if let Some(permit) = encoding.try_permit().await {
                        return crate::vodencode::SourceCopyPermitRead::Admitted(permit);
                    }
                    let now = Instant::now();
                    if now >= admission_deadline {
                        return crate::vodencode::SourceCopyPermitRead::Capacity;
                    }
                    tokio::time::sleep(
                        crate::transcode::ADMISSION_POLL.min(admission_deadline - now),
                    )
                    .await;
                }
            })
            .await;
        let permit = match admission {
            Ok(crate::vodencode::SourceCopyPermitRead::Admitted(permit)) => permit,
            Ok(crate::vodencode::SourceCopyPermitRead::Unavailable) => {
                return Err("Source physical admission policy is unavailable".into());
            }
            Ok(crate::vodencode::SourceCopyPermitRead::Capacity) => {
                return Err("Source physical VOD capacity is unavailable".into());
            }
            Err(_) => return Err("Source physical VOD admission timed out".into()),
        };
        let pending = tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            Box::pin(self.prepare_source_vod_rendition(source, assignment, settings, encoding)),
        )
        .await
        .map_err(|_| "Source VOD preparation timed out".to_owned())??;
        Ok(AdmittedSourceVodRendition { pending, permit })
    }

    /// Prepare only current opaque Source planning input; no Source viewer is
    /// attached here. Ordinary Shared creation continues to refuse.
    #[allow(dead_code)] // Consumed by the finite owned Source actor stage.
    pub(crate) async fn prepare_source_vod_rendition(
        &self,
        source: &crate::http::hls::PreparedSourcePlayback,
        assignment: &plurx_core::sharing_source_sessions::SourceDispatchAssignment,
        settings: &VodSettings,
        encoding: Option<Arc<crate::vodencode::Encoding>>,
    ) -> Result<PreparedSourceVodRendition, String> {
        if !source.matches_assignment(assignment) {
            return Err("Source prepared request differs from its immutable assignment".to_owned());
        }
        let mut file = source.file().clone();
        file.audio_offset_ms = if file.audio_streams.is_empty() {
            0
        } else {
            source.request().audio_offset_ms.clamp(-15_000, 15_000)
        };
        let fences = VodCreateFences {
            release_fence: None,
            serving_admission: None,
            expected_attempt: None,
            viewer: None,
        };
        let prepared = Box::pin(self.prepare_source_rendition(
            VodRecipeRequest {
                root_admission: None,
                request: source.request(),
                encoding,
                ..source.request().into()
            },
            &file,
            settings,
            &fences,
            Some(assignment.binding()),
        ))
        .await?;
        Ok(PreparedSourceVodRendition {
            prepared,
            request: source.request().clone(),
            file,
            settings: settings.clone(),
            assignment: assignment.clone(),
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn commit_source_rendition(
        &self,
        req: &SessionRequest,
        file: &MediaFile,
        settings: &VodSettings,
        attribution: VodAttribution<'_>,
        session_id: String,
        fences: VodCreateFences<'_>,
        prepared: PreparedVodRendition,
    ) -> Result<VodStart, String> {
        let PreparedVodRendition {
            attachment,
            start_entry,
            marker_destinations,
        } = prepared;
        let rendition = Arc::clone(&attachment.rendition);
        let _release_transition = if let Some(release_fence) = fences.release_fence {
            let guard = release_fence.transition.lock_owned().await;
            if release_fence.released.load(Acquire) {
                return Err(crate::transcode::vod_refusal_error(
                    "vod_session_released",
                    "the VOD session was released before attachment",
                ));
            }
            Some(guard)
        } else {
            None
        };
        let lifecycle = self.shared.session_lifecycle(&session_id);
        let _lifecycle = lifecycle.lock().await;
        let duration_ms = plan_duration_ms(&rendition.plan);
        let replacement = Session {
            children: Vec::new(),
            root_admission: None,
            processed_reuse_scope: None,
            processed_reuse_seed: None,
            preferred_start_ms: super::vod_serve_delivery::preferred_playlist_start_ms(
                req.start_seconds,
                duration_ms,
            ),
            passive_grant: None,
            retained_output: None,
            sdr_master_codecs: false,
            rendition: Some(Arc::clone(&rendition)),
            rendition_key: rendition.key.clone(),
            file: Arc::new(rendition.recipe.file.clone()),
            playback_id: req.playback_id.clone(),
            user_name: attribution.user_name.to_owned(),
            item_title: attribution.item_title.to_owned(),
            started_unix: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_secs().min(i64::MAX as u64) as i64)
                .unwrap_or(0),
            target_height: rendition
                .recipe
                .encoding
                .as_ref()
                .map_or(file.height.unwrap_or(0), |encoding| {
                    encoding.options.target_height
                }),
            kind: rendition
                .recipe
                .encoding
                .as_ref()
                .map_or(req.kind, |encoding| SessionKind::Transcode {
                    height: encoding.options.target_height,
                }),
            supersession_user: attribution.supersession_user.to_owned(),
            block_budget: settings.block_budget,
            lifecycle: Arc::clone(&lifecycle),
            incarnation: Arc::new(()),
            last_touch: StdMutex::new(Instant::now()),
            // The same decision as `kind` above: an encoded rendition is a
            // transcode whatever the request asked for.
            delivery: Arc::new(crate::meter::Meter::for_method(
                if rendition.recipe.encoding.is_some()
                    || matches!(req.kind, SessionKind::Transcode { .. })
                {
                    "transcode"
                } else {
                    "remux"
                },
            )),
            control: StdMutex::new(crate::playback_control::ControlState::default()),
            marker_destinations,
            control_observed_at: None,
            last_control_snapshot: None,
            control_end: None,
            control_end_snapshot: None,
            prepared_incarnation: None,
            #[cfg(target_os = "linux")]
            finite_session_slot: None,
            #[cfg(target_os = "linux")]
            finite_control_slot: None,
            terminal_cleanup: None,
            tombstone: None,
        };

        #[cfg(target_os = "linux")]
        let mut replacement = replacement;
        #[cfg(target_os = "linux")]
        replacement.reserve_finite_session_state(&session_id)?;

        // Acquire every async lock before changing either the reader graph or
        // the registry. Cancellation while resurrecting is therefore a clean
        // no-op; after the final lock is acquired the attachment replacement
        // is one synchronous transaction with no partial externally visible
        // state.
        let mut sessions = self.shared.sessions.lock().await;
        if sessions
            .get(&session_id)
            .is_some_and(|session| session.tombstone.is_some())
        {
            return Err(crate::transcode::vod_refusal_error(
                "vod_session_ended",
                "the VOD session already has a terminal tombstone",
            ));
        }
        let previous_rendition = sessions
            .get(&session_id)
            .and_then(|session| session.rendition.as_ref().map(Arc::clone));
        let mut previous_readers = match previous_rendition.as_ref() {
            Some(previous) if !Arc::ptr_eq(previous, &rendition) => {
                Some(previous.readers.lock().await)
            }
            _ => None,
        };
        // Set only where this attachment actually moves the viewer off another
        // rendition. Stopping the flight has to happen after the guards below
        // are dropped — settlement waits on a real ffmpeg, and holding a
        // rendition-wide lock across that would let one viewer's recipe change
        // stall every other reader of the same rendition — so what crosses the
        // drop is the flight's identity, not the decision to stop it.
        let mut obsolete_window_flight: Option<u64> = None;
        let mut replacement_readers = rendition.readers.lock().await;
        let _serving_transition = if let Some(admission) = fences.serving_admission.as_ref() {
            Some(admission.commit_guard_before().await.ok_or_else(|| {
                crate::transcode::serving_fence_error(crate::serving_fence::SERVING_FENCED_MESSAGE)
            })?)
        } else {
            None
        };
        // Construct and charge the new finite reader before removing the
        // predecessor or committing the session. The guarded Vec cannot grow
        // during the later synchronous insertion.
        if !replacement_readers.has_room_for(&session_id) {
            return Err("finite reader table has no remaining slot".into());
        }
        let prepared_reader = rendition.prepare_client_reader(
            &session_id,
            start_entry,
            crate::admission::Priority::Live,
        )?;
        if let Some(previous_readers) = previous_readers.as_mut() {
            previous_readers.remove(&session_id);
            if previous_readers.is_empty() {
                if let Some(previous) = previous_rendition.as_ref() {
                    *previous.dormant_since.lock().expect("dormant lock") = Some(Instant::now());
                }
            }
            // Reattachment removes the reader here rather than through
            // `detach_reader`, so it needs the same wait retirement: this
            // viewer has moved to another rendition, and any request still
            // parked on the one they left has no reader to be ranked against.
            // `playback_demands` would mark it foreground on exactly that
            // absence and keep the old rendition producing for somebody who
            // is no longer watching it.
            if let Some(previous) = previous_rendition.as_ref() {
                self.shared.pool.retire_session(&previous.key, &session_id);
            }
            // The third thing `detach_reader` does, which this path was
            // missing, and which it cannot do the same way. A subtitle window
            // is keyed by session id alone, so the flight this viewer left
            // behind is a live ffmpeg extracting a span for the recipe they
            // just moved off — worth stopping, and `detach_reader`'s comment
            // claims every ending converges here to stop it.
            //
            // It cannot call `release_session_window`, because releasing also
            // fences the id for half a minute and this id belongs to a viewer
            // who is still watching: the fence would refuse the first window
            // of the attachment replacing it and turn a recipe change into
            // half a minute without subtitles. It names the exact flight
            // instead, read here under the same guard that decides it is
            // obsolete, so a successor claiming the session between this read
            // and the stop is left alone.
            obsolete_window_flight = crate::subtitles::session_window_flight(&session_id);
        }
        replacement_readers
            .try_insert(prepared_reader.0, prepared_reader.1)
            .expect("preflighted reader insertion remains within guarded table");
        *rendition.dormant_since.lock().expect("dormant lock") = None;
        // A live entry with this id is replaced rather than refused, and the
        // replacement carries a fresh `ControlState` — so a staged M6
        // successor belonging to the outgoing attachment would simply vanish
        // while its durable row lived on. Abort it first, and note that the
        // replacement mints a new `incarnation`: a gate held for the outgoing
        // attachment is pinned to the old one and refuses everything from
        // here, rather than taking the newcomer's slot.
        if let Some(outgoing) = sessions.get(&session_id) {
            outgoing.abort_staged_preparation();
        }
        sessions.insert(session_id.clone(), replacement);
        // Both reader graphs are committed, so the rendition-wide guards have
        // no further work. They used to live to the end of the function, which
        // was free while nothing here awaited; the window stop below does, and
        // settlement waits on a real ffmpeg. Holding either guard across that
        // would let one viewer's recipe change stall every other reader of the
        // rendition they left or the one they joined.
        drop(replacement_readers);
        drop(previous_readers);
        drop(_serving_transition);
        drop(sessions);
        if let Some(flight) = obsolete_window_flight {
            crate::subtitles::abandon_session_window(&session_id, flight).await;
        }
        self.emit_lifecycle(
            &session_id,
            file.id,
            file.height.unwrap_or(0),
            req.kind,
            "session_start",
            None,
        );
        drop(_lifecycle);
        // The reader graph and registry are now one committed attachment.
        // Release does not wait for producer wakeup, tracing or lifecycle
        // event publication, and can tombstone this exact incarnation before
        // the caller attempts to acquire a response owner.
        drop(_release_transition);
        // Reader graph and registry now own the exact handle. Releasing the
        // per-key build gate before this point would let a dormant purge
        // remove it in the lookup/attach gap.
        drop(attachment);
        rendition.kick();
        tracing::info!(
            target: "plurxd::vodserve",
            session = %session_log_id(&session_id),
            rendition = %rendition.key,
            file = file.id,
            "vod session attached (start entry {start_entry})"
        );
        Ok(VodStart {
            session_id,
            duration_ms,
        })
    }

    pub(super) async fn prepare_source_rendition(
        &self,
        prepared: VodRecipeRequest<'_>,
        file: &MediaFile,
        settings: &VodSettings,
        fences: &VodCreateFences<'_>,
        source_binding: Option<&plurx_core::sharing_source_sessions::SourceBindingHandle>,
    ) -> Result<PreparedVodRendition, String> {
        // The one funnel every create passes through: the plain entry point,
        // the cluster one that every shipped caller actually uses, and the
        // resurrection of a session from its durable route. Applying the
        // ceiling here rather than at construction is what lets an operator
        // change it without a restart — a cap set only in `WaitPool::new`
        // would be frozen at whatever the node booted with — and applying it
        // at `try_create` alone reached no production path at all.
        self.shared.pool.set_global_cap(settings.blocked_get_cap);
        let source_binding = source_binding
            .ok_or_else(|| "Source rendition requires exact typed binding".to_owned())?;
        let source_binding = Some(source_binding);
        let req = prepared.request;
        let (aac, preserve_dolby_vision, convert_dolby_vision) = match req.kind {
            SessionKind::Copy {
                aac,
                preserve_dolby_vision,
                convert_dolby_vision,
                ..
            } if prepared.encoding.is_none() => (aac, preserve_dolby_vision, convert_dolby_vision),
            _ if prepared.encoding.is_some() => (
                true,
                prepared
                    .encoding
                    .as_ref()
                    .is_some_and(|encoding| encoding.preserves_processed_dv()),
                false,
            ),
            _ => {
                return Err(crate::transcode::vod_refusal_error(
                    "vod_recipe_unresolved",
                    "the encoded VOD request has no resolved encoder recipe",
                ))
            }
        };
        if req.subtitle_burn.is_some() && prepared.encoding.is_none() {
            return Err(crate::transcode::vod_refusal_error(
                "vod_recipe_unresolved",
                "a subtitle burn requires a resolved encoded VOD recipe",
            ));
        }
        // A NULL/unprobed duration cannot be described by a closed film-time
        // playlist. Refuse it honestly; the removed live presentation is not
        // a substitute.
        let Some(duration_ms) = file.duration_ms.filter(|ms| *ms > 0) else {
            return Err(crate::transcode::vod_refusal_error(
                "vod_source_unsupported",
                "the file has no probed duration, so no immutable plan can be built",
            ));
        };
        // Source SDR preparation already owns its actual engine capture. A
        // generic capability probe here would create an unowned child.
        let have_dovi = if source_binding.is_some() {
            false
        } else {
            crate::ffmpeg::has_dovi_rpu().await
        };
        // An encoded rendition never carries the source's parameter sets, so
        // only a copy waits on the census.
        let probe_json = if prepared.encoding.is_some() && source_binding.is_some() {
            // Encoded identity comes from the verified held-probe recipe;
            // source parameter sets are unused by that identity.
            Ok(None)
        } else if prepared.encoding.is_none() && source_binding.is_none() {
            crate::hevc_census::probe_json_for_copy(self.shared.store.as_ref(), file).await
        } else {
            self.shared.store.get_file_probe_json(file.id).await
        }
        .map_err(|error| format!("reading the file probe: {error}"))?;
        let video = copy_video_pipeline(
            file,
            probe_json.as_deref(),
            have_dovi,
            preserve_dolby_vision,
            convert_dolby_vision,
        );
        let identity = match prepared.encoding.as_ref() {
            Some(encoding) => encoding.identity(file, duration_ms as f64 / 1_000.0),
            None => crate::fragindex::identity_for(file, video),
        };
        let cluster_cache_enabled = prepared.encoding.is_none()
            && self
                .shared
                .store
                .get_setting(plurx_core::store::keys::VOD_INDEX_CLUSTER_CACHE)
                .await
                .map_err(|error| format!("reading the cluster index gate: {error}"))?
                .is_some_and(|value| {
                    matches!(
                        value.trim().to_ascii_lowercase().as_str(),
                        "1" | "true" | "yes" | "on"
                    )
                });
        let cluster_index = if prepared.encoding.is_some() || source_binding.is_some() {
            Ok(None)
        } else if cluster_cache_enabled {
            self.try_cluster_fragment_index(file, video, fences.viewer.as_ref())
                .await
        } else {
            Ok(None)
        };
        let needs_attestation = cluster_cache_enabled && matches!(&cluster_index, Ok(None));
        let unavailable_reason = cluster_index.as_ref().err().cloned();
        let (index, source_object_version, cluster_cache_key) = match cluster_index {
            Ok(Some((index, object_version, cache_key))) => {
                (Some(index), Some(object_version), Some(cache_key))
            }
            Ok(None) if prepared.encoding.is_some() => (
                None,
                prepared
                    .encoding
                    .as_ref()
                    .map(|encoding| encoding.source_object_version.clone()),
                None,
            ),
            Ok(None) => (
                self.shared
                    .store
                    .fragment_index(file.id, &identity)
                    .await
                    .map_err(|error| format!("reading the fragment index: {error}"))?,
                None,
                None,
            ),
            Err(reason) => {
                // The first rollout phase is write/shadow plus prefer-v2.
                // Per-key fallback preserves an already healthy v1 title
                // until this exact source/pipeline key is fully available.
                tracing::debug!(target: "plurxd::vodserve", file_id = file.id, %reason, "v2 fragment index unavailable; using v1");
                (
                    self.shared
                        .store
                        .fragment_index(file.id, &identity)
                        .await
                        .map_err(|error| format!("reading the fragment index: {error}"))?,
                    None,
                    None,
                )
            }
        };
        // HEVC safety is established on original headers, before filters can
        // hide updates. Bind the proof to this node's current source object;
        // a peer's filesystem identity is not a local attestation.
        // A copy that keeps its in-band parameter sets cannot decode against
        // stale definitions, so it needs no proof that deleting them is safe.
        let source_object_version = if prepared.encoding.is_none()
            && matches!(file.video_codec.as_deref(), Some("hevc" | "h265"))
            && !video.retains_hevc_parameter_sets()
            && !settings.hevc_unverified_copy
        {
            let current = crate::fragment_index_cluster::inspect_source(file)
                .await
                .map_err(|reason| {
                    crate::transcode::vod_refusal_error("hevc_configuration_unverified", reason)
                })?;
            let proof = index
                .as_ref()
                .and_then(|index| index.promotion.hevc_configuration.as_ref());
            if !proof.is_some_and(|proof| {
                proof.permits_on_node(
                    &current,
                    self.shared.cluster_node_id.as_deref().unwrap_or_default(),
                )
            }) {
                let reason = proof
                    .filter(|proof| {
                        proof.source_object_version == current
                            && Some(proof.source_node_id.as_str())
                                == self.shared.cluster_node_id.as_deref()
                    })
                    .and_then(|proof| proof.refusal.as_deref());
                let mut preparation = if cluster_cache_enabled {
                    "HEVC copy needs preparation".to_owned()
                } else {
                    "HEVC copy needs preparation; shared preparation is disabled".to_owned()
                };
                if reason.is_none() && cluster_cache_enabled && source_binding.is_none() {
                    if let Some(node) = self.shared.cluster_node_id.as_deref() {
                        preparation =
                            match crate::state::enqueue_copy_preparation_for_object_with_viewer(
                                self.shared.store.as_ref(),
                                node,
                                file,
                                video,
                                Some(&current),
                                fences.viewer.as_ref(),
                            )
                            .await
                            {
                                Ok(request) => {
                                    format!("HEVC exact copy preparation is {}", request.state)
                                }
                                Err(error) => {
                                    format!("HEVC copy preparation could not be queued: {error}")
                                }
                            };
                    }
                }
                let detail = reason.map(str::to_owned).unwrap_or_else(|| {
                    format!(
                    "{preparation}; Settings → Developer can enable unverified copy without waiting"
                )
                });
                return Err(crate::transcode::vod_refusal_error(
                    if reason.is_some() {
                        "hevc_configuration_unsupported"
                    } else {
                        "hevc_configuration_unverified"
                    },
                    detail,
                ));
            }
            Some(current)
        } else {
            source_object_version
        };
        if index.is_none() && prepared.encoding.is_none() {
            let reason = if needs_attestation && source_binding.is_none() {
                match self.shared.cluster_node_id.as_deref() {
                    Some(node_id) => {
                        match crate::state::enqueue_copy_preparation_for_object_with_viewer(
                            self.shared.store.as_ref(),
                            node_id,
                            file,
                            video,
                            None,
                            fences.viewer.as_ref(),
                        )
                        .await
                        {
                            Ok(request) => format!(
                                "exact copy preparation is {}{}",
                                request.state,
                                if request.last_error_code.is_empty() {
                                    String::new()
                                } else {
                                    format!(": {}", request.last_error_code)
                                }
                            ),
                            Err(error) => {
                                format!("exact copy preparation could not be queued: {error}")
                            }
                        }
                    }
                    None => "this process has no cluster index identity".to_owned(),
                }
            } else {
                // The node-local pass builds this index; what its last
                // attempt found decides the refusal (`local_index_refusal`).
                let (code, reason) = self
                    .shared
                    .local_index_refusal(
                        file.id,
                        &identity,
                        unavailable_reason.unwrap_or_else(|| {
                            "shared preparation is disabled; no matching local index exists"
                                .to_owned()
                        }),
                    )
                    .await;
                return Err(crate::transcode::vod_refusal_error(code, reason));
            };
            // This is a prerequisite refusal, not a claim that a worker is
            // active. The durable analysis row and reason carry its actual
            // queued/running/failed state; the caller keeps rolling first play.
            return Err(crate::transcode::vod_refusal_error(
                "vod_index_pending",
                reason,
            ));
        }
        // The §2 ruling: a single immutable init cannot describe a film whose
        // clean fragments carry varying parameter sets, so the verdict is a
        // scan-time fallback here, never a producer_failed mid-playback.
        if video.retains_hevc_parameter_sets()
            && index
                .as_ref()
                .is_some_and(|index| !index.promotion.dvh1_parameter_sets.is_empty())
        {
            return Err(crate::transcode::vod_refusal_error(
                "vod_source_unsupported",
                "dvh1 completeness repair cannot retain in-band parameter sets",
            ));
        }
        if index
            .as_ref()
            .is_some_and(|index| !index.parameter_sets_constant)
        {
            return Err(crate::transcode::vod_refusal_error(
                "vod_source_unsupported",
                "its parameter sets vary mid-film (the §2 ruling)",
            ));
        }
        let recipe = Recipe {
            measured_candidate: None,
            retained_logical: None,
            audio_delivery: prepared
                .encoding
                .as_ref()
                .and_then(|encoding| encoding.options.audio.clone())
                .or_else(|| req.audio_delivery.clone()),
            file: file.clone(),
            audio_index: req.audio_index,
            aac,
            video,
            source_object_version,
            cluster_cache_key,
            encoding: prepared.encoding,
        };
        let key = source_binding.map_or_else(
            || rendition_key(&recipe, &identity),
            |binding| source_rendition_key(binding, &recipe, &identity),
        );
        let attachment = self
            .shared
            .attach_rendition_with_storage(
                &key,
                &identity,
                index,
                recipe,
                duration_ms,
                settings,
                None,
                None,
            )
            .await?
            .ok_or_else(|| {
                crate::transcode::vod_refusal_error(
                    "vod_source_unsupported",
                    "the fragment index produced an empty VOD plan",
                )
            })?;
        let rendition = Arc::clone(&attachment.rendition);

        let start_entry = entry_containing(&rendition.plan, req.start_seconds);
        let marker_destinations = stored_marker_destinations(
            self.shared.store.as_ref(),
            file,
            &rendition.plan,
            rendition.seconds_per_segment,
        )
        .await;
        Ok(PreparedVodRendition {
            attachment,
            start_entry,
            marker_destinations,
        })
    }
}

#[cfg(test)]
impl VodServe {
    /// Observe the same cached M-backed Encoding only after the actual Source
    /// assignment's dispatch/child associations and reader demand retired.
    /// Weak observation adds no metadata/application owner or close witness.
    #[cfg(target_os = "linux")]
    pub(crate) async fn settled_source_recipe_for_actor_test(
        &self,
        assignment: &plurx_core::sharing_source_sessions::SourceDispatchAssignment,
    ) -> (
        std::path::PathBuf,
        std::sync::Weak<crate::vodencode::Encoding>,
    ) {
        let originals: Vec<_> = self
            .shared
            .renditions
            .lock()
            .await
            .values()
            .filter(|rendition| {
                rendition
                    .source_owners
                    .settled_assignment_for_actor_test(assignment)
            })
            .cloned()
            .collect();
        assert_eq!(originals.len(), 1, "one original settled Source recipe");
        let rendition = &originals[0];
        assert!(rendition.key.starts_with("source-"));
        assert!(
            rendition.readers.lock().await.is_empty(),
            "actual Source readers must retire before cache eviction"
        );
        assert!(
            rendition
                .dormant_since
                .lock()
                .expect("dormant lock")
                .is_some(),
            "the original recipe is already dormant"
        );
        let encoding = rendition
            .recipe
            .encoding
            .as_ref()
            .expect("original encoded Source recipe");
        assert!(
            encoding.planning_metadata.is_some(),
            "the cached original Encoding still owns its original M report"
        );
        (rendition.dir.path().to_owned(), Arc::downgrade(encoding))
    }

    /// Exercise the existing actual cache owner's eviction callback. Its
    /// original gates/readers/pool/producer/refcount/path-identity checks still
    /// decide eligibility; this fixture does not fabricate capacity pressure.
    #[cfg(target_os = "linux")]
    pub(crate) async fn pressure_evict_source_recipe_for_actor_test(
        &self,
        path: &std::path::Path,
    ) -> std::io::Result<bool> {
        super::cache_budget::pressure_evict_test(&self.shared, path).await
    }

    /// Move only the fixture's actual last-media clock past the existing idle
    /// allowance, then run the unmodified production maintenance/reap path.
    pub(crate) async fn expire_source_viewer_for_actor_test(
        &self,
        session_id: &str,
        assignment: &plurx_core::sharing_source_sessions::SourceDispatchAssignment,
    ) {
        {
            let sessions = self.shared.sessions.lock().await;
            let session = sessions.get(session_id).expect("actual Source viewer");
            assert_eq!(
                session.supersession_user,
                assignment.binding().principal().owner_key()
            );
            assert!(session
                .live_rendition()
                .expect("live Source rendition")
                .key
                .starts_with("source-"));
            *session.last_touch.lock().expect("actual media clock") =
                Instant::now() - SESSION_IDLE_TTL - Duration::from_secs(1);
        }
        self.maintain().await;
    }
}

#[cfg(test)]
mod principal_demand_tests {
    use super::*;
    use plurx_core::domain::ProbeResult;
    use plurx_core::domain::{ItemKind, LibraryKind, NewItem, NewLibrary};
    use plurx_core::playback_principal::PlaybackPrincipal;
    use plurx_core::store::SqliteStore;

    #[test]
    fn processed_instance_storage_is_frozen_per_admission_and_separate_from_recipe() {
        let canonical = "a".repeat(64);
        let first = processed_instance_key(&canonical, "incarnation-a", [1; 16]);
        assert_eq!(first.len(), 64);
        assert!(first.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_eq!(
            first,
            processed_instance_key(&canonical, "incarnation-a", [1; 16]),
            "retry of one admitted preparation keeps its private storage"
        );
        assert_ne!(first, canonical, "canonical recipe remains separate");
        assert_ne!(
            first,
            processed_instance_key(&canonical, "incarnation-a", [2; 16])
        );
        assert_ne!(
            first,
            processed_instance_key(&canonical, "incarnation-b", [1; 16])
        );
        assert_ne!(
            first,
            processed_instance_key(&"b".repeat(64), "incarnation-a", [1; 16])
        );
    }

    #[tokio::test]
    async fn shared_vod_demand_refuses_before_pool_queue_or_session_allocation() {
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let library = store
            .create_library(&NewLibrary {
                name: "Demand refusal".to_owned(),
                kind: LibraryKind::Movies,
                paths: Vec::new(),
                anime: false,
            })
            .await
            .expect("library");
        let item = store
            .insert_item(&NewItem {
                library_id: library.id,
                kind: ItemKind::Movie,
                parent_id: None,
                title: "Absent source".to_owned(),
                year: None,
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("item");
        let file_id = store
            .upsert_file(
                item,
                "/absent/shared-demand.mkv",
                100,
                1,
                &ProbeResult::default(),
            )
            .await
            .expect("file");
        let file = store
            .get_file(file_id)
            .await
            .expect("read file")
            .expect("file row");
        let root = crate::test_tempdir().expect("VOD root");
        let serve = VodServe::new_cluster(
            root.path().to_path_buf(),
            store.clone(),
            "node-a".to_owned(),
            root.path().join("index"),
            None,
        );
        let request = SessionRequest {
            quality_catalog: None,
            candidate_context: None,
            control_sequence: None,
            file_id,
            playback_id: "shared-demand".to_owned(),
            request_id: None,
            automatic: false,
            previous_session_id: None,
            reopen_reason: None,
            kind: SessionKind::Copy {
                aac: true,
                preserve_dolby_vision: false,
                convert_dolby_vision: false,
                dual_layer_as_copied: false,
            },
            start_seconds: 0.0,
            audio_index: None,
            subtitle_burn: None,
            audio_offset_ms: 0,
            hdr10: false,
            presentation: Default::default(),
            block_budget_secs: None,
            transport: None,
            continuous_media: None,
            passive_vod: false,
            vod_only: false,
            finite_bitrate_limit_bps: None,
            audio_claim: None,
            audio_delivery: None,
            dv_processing_destination: None,
            sdr_master_codecs: None,
        };
        let initial_cap = serve.shared.pool.global_cap();
        let settings = VodSettings {
            working_set_bytes: 8 << 30,
            completed_cache_bytes: 50 << 30,
            block_budget: Duration::from_secs(30),
            materialize_budget: Duration::from_secs(30),
            blocked_get_cap: initial_cap + 7,
            sdr_master_codecs: false,
            index_cluster_cache: false,
            hevc_unverified_copy: false,
            live_recovery: true,
            output_preparation: OutputPreparation::Off,
            output_budget_bytes: 50 << 30,
        };
        let viewer = crate::state::PlaybackViewerDemand {
            principal: PlaybackPrincipal::sharing(uuid::Uuid::new_v4(), &"a".repeat(64))
                .expect("sharing principal"),
            playback_id: request.playback_id.clone(),
        };
        let refusal = serve
            .try_create_for_viewer(
                &request,
                &file,
                &settings,
                VodAttribution {
                    user_name: "recipient viewer",
                    item_title: "Absent source",
                    supersession_user: "shared-demand",
                },
                "00000000-0000-4000-a000-000000000190".to_owned(),
                viewer.clone(),
            )
            .await
            .expect_err("Shared create needs source admission");
        assert_eq!(
            refusal,
            "sharing playback demand requires typed source authority"
        );
        let index_refusal = serve
            .try_cluster_fragment_index(&file, CopyVideoOptions::new(true, false), Some(&viewer))
            .await
            .expect_err("Shared index needs source admission");
        assert_eq!(index_refusal, refusal);
        assert_eq!(serve.shared.pool.global_cap(), initial_cap);
        assert!(serve
            .shared
            .preparing_sessions
            .lock()
            .expect("preparing sessions")
            .is_empty());
        assert!(serve.shared.sessions.lock().await.is_empty());
        assert!(serve.shared.renditions.lock().await.is_empty());
        assert!(store
            .analysis_requests(10)
            .await
            .expect("no preparation queue")
            .is_empty());
    }
}
