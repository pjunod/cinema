use super::*;

impl VodServe {
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
            .filter(|(_, session)| session.tombstone.is_none())
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
    async fn prepare_rendition(
        &self,
        prepared: VodRecipeRequest<'_>,
        file: &MediaFile,
        settings: &VodSettings,
        viewer: Option<&crate::state::PlaybackViewerDemand>,
    ) -> Result<RenditionAttachment, String> {
        // The one funnel every create passes through: the plain entry point,
        // the cluster one that every shipped caller actually uses, and the
        // resurrection of a session from its durable route. Applying the
        // ceiling here rather than at construction is what lets an operator
        // change it without a restart — a cap set only in `WaitPool::new`
        // would be frozen at whatever the node booted with — and applying it
        // at `try_create` alone reached no production path at all.
        self.shared.pool.set_global_cap(settings.blocked_get_cap);
        let req = prepared.request;
        let (aac, preserve_dolby_vision, convert_dolby_vision) = match req.kind {
            SessionKind::Copy {
                aac,
                preserve_dolby_vision,
                convert_dolby_vision,
            } if prepared.encoding.is_none() => (aac, preserve_dolby_vision, convert_dolby_vision),
            _ if prepared.encoding.is_some() => (true, false, false),
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
        let have_dovi = crate::ffmpeg::has_dovi_rpu().await;
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
            && !crate::transcode::unverified_hevc_copy_enabled(self.shared.store.as_ref()).await?
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
        let recipe = Recipe {
            file: file.clone(),
            audio_index: req.audio_index,
            aac,
            video,
            source_object_version,
            cluster_cache_key,
            encoding: prepared.encoding,
        };
        let key = rendition_key(&recipe, &identity);
        self.shared
            .attach_rendition(&key, &identity, index, recipe, duration_ms, settings)
            .await?
            .ok_or_else(|| {
                crate::transcode::vod_refusal_error(
                    "vod_source_unsupported",
                    "the fragment index produced an empty VOD plan",
                )
            })
    }

    async fn reserve_media_group(
        media: &[Arc<Rendition>],
    ) -> Result<Vec<crate::vodencode::EncodePermit>, String> {
        let refuse = || {
            crate::transcode::vod_refusal_error(
                "vod_family_capacity",
                "the continuous media group cannot be admitted together",
            )
        };
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
        let total_cpu = estimates
            .iter()
            .try_fold(0usize, |sum, estimate| {
                sum.checked_add(estimate.cpu_threads)
            })
            .ok_or_else(refuse)?;
        if total_cpu > software_budget
            || estimates
                .iter()
                .filter(|estimate| estimate.hardware_slot)
                .count()
                > hardware_limit
        {
            return Err(refuse());
        }
        let mut retained = media
            .iter()
            .map(|rendition| rendition.retained_admission.current())
            .collect::<Vec<_>>();
        let missing = estimates
            .iter()
            .zip(&retained)
            .filter_map(|(estimate, permit)| permit.is_none().then_some(*estimate))
            .collect::<Vec<_>>();
        let priority = if encodings
            .iter()
            .any(|encoding| encoding.is_speculative() || encoding.nonpreemptive_trial)
        {
            crate::admission::Priority::Speculative
        } else {
            crate::admission::Priority::Live
        };
        let mut admitted = if missing.is_empty() {
            Vec::new()
        } else {
            first
                .admissions
                .try_admit_bundles_claiming(
                    hardware_limit,
                    software_budget,
                    &missing,
                    priority,
                    None,
                )
                .ok_or_else(refuse)?
        }
        .into_iter();
        for (rendition, permit) in media.iter().zip(&mut retained) {
            if permit.is_none() {
                *permit = Some(
                    rendition
                        .retained_admission
                        .bind(admitted.next().expect("one permit per missing role").into()),
                );
            }
        }
        Ok(retained
            .into_iter()
            .map(|permit| permit.expect("every role admitted"))
            .collect())
    }

    async fn try_create_with_release_fence(
        &self,
        prepared: VodRecipeRequest<'_>,
        file: &MediaFile,
        settings: &VodSettings,
        attribution: VodAttribution<'_>,
        session_id: String,
        fences: VodCreateFences<'_>,
    ) -> Result<VodStart, String> {
        let req = prepared.request;
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
                    .prepare_rendition(
                        VodRecipeRequest {
                            request,
                            encoding: Some(Arc::clone(encoding)),
                            soundtrack: None,
                            companion: None,
                        },
                        file,
                        settings,
                        fences.viewer.as_ref(),
                    )
                    .await?;
                let rendition = Arc::clone(&attachment.rendition);
                drop(attachment);
                Some(rendition)
            }
            None => None,
        };
        // Prepare independently, then reacquire all build gates in key order.
        // No public reader exists until the exact cached objects are rechecked.
        let mut audio_request = req.clone();
        if let Some(media) = audio_request.continuous_media.as_mut() {
            media.role = crate::transcode::ContinuousMediaRole::SharedAudio;
            media.autonomous_companion = None;
            media.companion_context = None;
        }
        let audio_rendition = match prepared.soundtrack.as_ref() {
            Some(soundtrack) => {
                let attachment = self
                    .prepare_rendition(
                        VodRecipeRequest {
                            companion: None,
                            request: &audio_request,
                            encoding: Some(Arc::clone(soundtrack)),
                            soundtrack: None,
                        },
                        file,
                        settings,
                        fences.viewer.as_ref(),
                    )
                    .await?;
                let rendition = Arc::clone(&attachment.rendition);
                drop(attachment);
                Some(rendition)
            }
            None => None,
        };
        let attachment = self
            .prepare_rendition(prepared, file, settings, fences.viewer.as_ref())
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
            let permits = Self::reserve_media_group(&media).await?;
            media
                .into_iter()
                .zip(permits)
                .map(|(child, permit)| ParentMediaReader {
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
        let mut replacement = Session {
            children: Vec::new(),
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
        let outgoing_children = sessions
            .get_mut(&session_id)
            .map(|session| std::mem::take(&mut session.children))
            .unwrap_or_default();
        sessions.insert(session_id.clone(), replacement);
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
