use super::*;

/// One request's resolved native recipe, before any rendition is attached.
struct PreparedVodRecipe {
    identity: SourceIdentity,
    index: Option<FragmentIndex>,
    recipe: Recipe,
    duration_ms: i64,
    incoming_logical: Option<crate::vodserve::retained_manifest::LogicalOutput>,
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
        let canonical_key = rendition_key(&recipe, &identity);
        let key = private_preparation.as_ref().map_or_else(
            || canonical_key.clone(),
            |nonce| {
                let mut hash = Sha256::new();
                hash.update(b"plurx:private-copy-preparation-incarnation:v1\0");
                hash.update(canonical_key.as_bytes());
                hash.update(nonce.nonce.as_bytes());
                hex::encode(hash.finalize())
            },
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
                private_preparation,
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
        let have_dovi = crate::ffmpeg::has_dovi_rpu().await;
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
                unavailable_reason.unwrap_or_else(|| {
                    "shared preparation is disabled; no matching local index exists".to_owned()
                })
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
            first.store.get_setting_pair(
                plurx_core::store::keys::MAX_HW_SESSIONS,
                plurx_core::store::keys::SW_POOL_THREADS,
            ),
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
        let views = retained
            .iter()
            .map(|permit| {
                permit
                    .as_ref()
                    .map(crate::vodencode::EncodePermit::retained_resources)
            })
            .collect::<Vec<_>>();
        let (family, admitted) = first
            .admissions
            .try_admit_family(
                hardware_limit,
                software_budget,
                &estimates,
                &views,
                priority,
            )
            .ok_or_else(refuse)?;
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
        Ok(retained
            .into_iter()
            .map(|permit| permit.expect("every role admitted"))
            .collect())
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
            .resolve_rendition(&mut prepared, file, settings, fences.viewer.as_ref(), None)
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
            let permits = Self::reserve_media_group(&media, None).await?;
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

        let start_entry = entry_containing(&rendition.plan, req.start_seconds);
        let retained_output = match prepared.retained_capture {
            RetainedOutputCapture::Restore(Some(ref expected)) => Some(
                self.shared
                    .retained_artifacts
                    .reacquire_expected_for_request(
                        expected,
                        &self.shared,
                        &rendition,
                        &incoming_logical,
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
                    .filter(|artifact| artifact.logical == incoming_logical);
                if existing.is_some() {
                    existing
                } else if req.candidate_context.is_some() {
                    self.shared
                        .retained_artifacts
                        .acquire_prepared_candidate(&rendition, &incoming_logical, file, req)
                        .await
                } else {
                    self.shared
                        .retained_artifacts
                        .acquire_prepared_manual(&rendition, &incoming_logical, file)
                        .await
                }
            }
        };
        let marker_destinations = stored_marker_destinations(
            self.shared.store.as_ref(),
            file,
            &rendition.plan,
            rendition.seconds_per_segment,
        )
        .await;
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
            terminal_cleanup: None,
            tombstone: None,
        };

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
                replacement_readers.insert(child.reader_id.clone(), Reader::new(entry));
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
                readers.insert(child.reader_id.clone(), Reader::new(entry));
                *child.rendition.dormant_since.lock().expect("dormant lock") = None;
            }
        }
        replacement.children = std::mem::take(&mut private_media);
        let mut authority = Reader::new(start_entry);
        authority.authority_only = replacement.children.iter().any(|child| child.controlled);
        replacement_readers.insert(session_id.clone(), authority);
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
                        encoding.options.pipeline.output_grade()
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
            .arm_materialize_watchdog(&self.rendition, INIT_DEMAND_INDEX);
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
            if demand.expired() {
                return Err("materializing init.mp4 exceeded the producer deadline".into());
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
            rendition.slot.belief().await,
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
            viewer: None,
        };
        let prepared = Box::pin(self.prepare_source_rendition(
            VodRecipeRequest {
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
            terminal_cleanup: None,
            tombstone: None,
        };

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
        replacement_readers.insert(session_id.clone(), Reader::new(start_entry));
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
                unavailable_reason.unwrap_or_else(|| {
                    "shared preparation is disabled; no matching local index exists".to_owned()
                })
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
