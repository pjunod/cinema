use super::*;

impl TranscodeManager {
    /// The claimed worker distinguishes a proved missing/changed row from a
    /// Store failure. The latter must remain retryable, not terminal authority.
    pub(crate) async fn claimed_preparation_file(
        store: &dyn Store,
        file_id: i64,
        source_size: i64,
        source_mtime: i64,
        fence: &crate::background_jobs::JobFence,
    ) -> Result<Option<plurx_core::domain::MediaFile>, String> {
        match store
            .get_file(file_id)
            .await
            .map_err(|error| error.to_string())?
        {
            Some(file) if file.size == source_size && file.mtime == source_mtime => Ok(Some(file)),
            _ => {
                fence
                    .settle(plurx_core::store::background_jobs::JobSettlement::Stop {
                        error_code: "source_changed".into(),
                    })
                    .await
                    .map_err(|error| error.to_string())?;
                Ok(None)
            }
        }
    }

    /// Only an already accepted canonical context can become a queued carrier.
    /// Absence remains byte-for-byte legacy/manual serialization, not authority.
    pub(crate) async fn queued_candidate_catalog(
        &self,
        request: &SessionRequest,
        file: &plurx_core::domain::MediaFile,
        node: &str,
    ) -> Result<Option<serde_json::Value>, String> {
        let Some(context) = request.candidate_context.as_ref() else {
            return Ok(None);
        };
        let catalog = crate::media_sessions::CandidateCatalogContext {
            caps: context
                .canonical_caps
                .clone()
                .ok_or("queued candidate caps missing")?,
            candidate: context.selected_candidate.clone(),
            binding: context
                .planning_binding
                .clone()
                .ok_or("queued candidate binding missing")?,
        };
        if context.owner_node_id.as_deref() != Some(node)
            || self.cache_location().map(|(_, owner)| owner) != Some(node)
            || catalog.candidate.id != context.candidate_id
            || catalog.candidate.recipe_digest != context.recipe_digest
            || catalog.candidate.grade != context.grade
            || catalog.candidate.normalized_geometry != context.normalized_geometry
            || context.planning_snapshot.as_ref().is_some_and(|snapshot| {
                snapshot.file.id != file.id
                    || snapshot.file.size != file.size
                    || snapshot.file.mtime != file.mtime
                    || crate::media_pool::PlanningBinding::from_snapshot(snapshot)
                        != catalog.binding
            })
        {
            return Err("queued candidate authority mismatch".into());
        }
        let restored = self
            .restore_catalog_context(
                request,
                &catalog,
                context.candidate_id,
                file.size,
                file.mtime,
                None,
                crate::media_pool::create_stage_deadline(Duration::from_secs(2)),
            )
            .await
            .map_err(|error| error.to_string())?;
        if restored.profile != context.profile {
            return Err("queued candidate profile changed".into());
        }
        serde_json::to_value(catalog)
            .map(Some)
            .map_err(|error| error.to_string())
    }

    async fn stop_unverifiable_candidate(
        fence: &crate::background_jobs::JobFence,
    ) -> Result<bool, String> {
        fence
            .settle(plurx_core::store::background_jobs::JobSettlement::Stop {
                error_code: "candidate_authority_stale_or_missing".into(),
            })
            .await
            .map_err(|error| error.to_string())?;
        Ok(false)
    }

    /// Reconstruct real process-local authority from the strict persisted
    /// carrier. A backend failure retries; incompatible evidence stops only
    /// this fenced task and never enters the producer.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn restore_queued_candidate(
        &self,
        request: &mut SessionRequest,
        file: &plurx_core::domain::MediaFile,
        catalog: Option<&serde_json::Value>,
        expected: Option<(plurx_core::playback::candidate::CandidateId, [u8; 32])>,
        node: &str,
        fence: &crate::background_jobs::JobFence,
        deadline: Instant,
    ) -> Result<bool, String> {
        let Some(encoded) = catalog else {
            return Self::stop_unverifiable_candidate(fence).await;
        };
        let Ok(catalog) = serde_json::from_value::<crate::media_sessions::CandidateCatalogContext>(
            encoded.clone(),
        ) else {
            return Self::stop_unverifiable_candidate(fence).await;
        };
        if self.cache_location().map(|(_, owner)| owner) != Some(node)
            || expected.is_some_and(|(id, digest)| {
                catalog.candidate.id != id || catalog.candidate.recipe_digest != digest
            })
        {
            return Self::stop_unverifiable_candidate(fence).await;
        }
        match self
            .restore_catalog_context(
                request,
                &catalog,
                catalog.candidate.id,
                file.size,
                file.mtime,
                None,
                tokio::time::Instant::from_std(deadline).min(
                    crate::media_pool::create_stage_deadline(Duration::from_secs(2)),
                ),
            )
            .await
        {
            Ok(mut context) => {
                context.owner_node_id = Some(node.to_owned());
                request.candidate_context = Some(Box::new(context));
                Ok(true)
            }
            Err(error) if error.is_incompatible() => Self::stop_unverifiable_candidate(fence).await,
            Err(error) => Err(error.to_string()),
        }
    }

    #[cfg(test)]
    pub(crate) async fn resolve_encoded_output_test(
        &self,
        request: &SessionRequest,
        file: &plurx_core::domain::MediaFile,
    ) -> Result<Arc<crate::vodencode::Encoding>, String> {
        self.prepare_vod_encoding(request, file)
            .await?
            .ok_or("encoded plan unavailable".to_owned())
    }
    fn encoded_output_intent(
        &self,
        request: &SessionRequest,
        encoding: &crate::vodencode::Encoding,
        node: &str,
    ) -> Result<plurx_core::store::background_jobs::EncodedOutputIntent, String> {
        use plurx_core::store::background_jobs::{CopyOutputProfile, EncodedOutputIntent};
        let output = encoding.plan.output_contract();
        let context = request.candidate_context.as_ref();
        Ok(EncodedOutputIntent {
            target_node_id: node.to_owned(),
            target_height: u32::try_from(encoding.options.target_height)
                .map_err(|_| "encoded height unavailable")?,
            requested_height: match request.kind {
                SessionKind::Transcode { height } => {
                    Some(u32::try_from(height).map_err(|_| "requested height unavailable")?)
                }
                _ => None,
            },
            copy_for_burn: match request.kind {
                SessionKind::Copy {
                    aac,
                    preserve_dolby_vision,
                    convert_dolby_vision,
                } => Some((aac, preserve_dolby_vision, convert_dolby_vision)),
                _ => None,
            },
            audio_index: request.audio_index,
            audio_offset_ms: request.audio_offset_ms,
            audio_claim: request
                .audio_claim
                .clone()
                .ok_or("encoded audio claim unavailable")?,
            audio_delivery: encoding
                .options
                .audio
                .clone()
                .ok_or("encoded audio delivery unavailable")?,
            subtitle_burn: request.subtitle_burn,
            subtitle_digest: encoding.subtitle_digest.clone(),
            hdr10_requested: request.hdr10,
            grade: encoding.options.pipeline.output_grade(),
            normalized_geometry: context.is_some_and(|context| context.normalized_geometry),
            profile: context
                .and_then(|context| context.profile)
                .map(|profile| match profile {
                    plurx_core::transcode::AutoQualityRateProfile::H264Sdr1440P30V1 => {
                        CopyOutputProfile::H264Sdr1440P30V1
                    }
                }),
            width: output
                .effective_width()
                .ok_or("encoded output width unavailable")?,
            height: output
                .effective_height()
                .ok_or("encoded output height unavailable")?,
            plan_digest: encoding.plan.plan_digest().to_owned(),
            executable_digest: encoding.executable.digest.clone(),
            engine_digest: encoding.engine.digest.clone(),
            candidate_id: context.map(|context| context.candidate_id),
            candidate_digest: context.map(|context| context.recipe_digest),
        })
    }

    pub(crate) async fn enqueue_encoded_output(
        &self,
        request: &SessionRequest,
        file: &plurx_core::domain::MediaFile,
        settings: &crate::vodserve::VodSettings,
        encoding: &crate::vodencode::Encoding,
    ) -> Result<(), String> {
        use plurx_core::store::background_jobs::{EnqueueJob, JobPayload, JobRequest};
        let node = request
            .candidate_context
            .as_ref()
            .and_then(|context| context.owner_node_id.as_deref())
            .or_else(|| self.cache_location().map(|(_, node)| node))
            .ok_or("encoded owner unavailable")?;
        let intent = self.encoded_output_intent(request, encoding, node)?;
        if !intent.valid() {
            return Err("encoded intent unavailable".to_owned());
        }
        let source = crate::fragment_index_cluster::open_source_fence(file, None).await?;
        if !source.unchanged() || source.object_version() != encoding.source_object_version {
            return Err("encoded source changed before queue publication".to_owned());
        }
        // Allocation budget, never measured wire cost. Actual writes remain
        // bounded by the same full-footprint preparation reservation.
        let cap = u64::try_from(file.size)
            .ok()
            .and_then(|bytes| bytes.checked_mul(2))
            .and_then(|bytes| bytes.checked_add(64 * 1024 * 1024))
            .map(|bytes| bytes.min(settings.completed_cache_bytes))
            .filter(|bytes| *bytes > 0)
            .ok_or("encoded retention cap unavailable")?;
        let payload = JobPayload::EncodedOutputPrepare {
            encoded_output_version: 1,
            file_id: file.id,
            source_generation: source.object_version().to_owned(),
            source_size: file.size,
            source_mtime: file.mtime,
            source_object_version: source.object_version().to_owned(),
            policy_generation: crate::vodserve::retained::encoded_policy_generation(file, &intent)
                .ok_or("encoded source metadata unavailable")?,
            candidate_catalog: self.queued_candidate_catalog(request, file, node).await?,
            intent,
            scratch_bytes: i64::try_from(cap).map_err(|_| "encoded cap overflow")?,
            reason: "recent_demand".to_owned(),
        };
        let digest = hex::encode(Sha256::digest(
            serde_json::to_vec(&payload).map_err(|error| error.to_string())?,
        ));
        let now = crate::media_sessions::unix_ms();
        self.store
            .enqueue_job(EnqueueJob {
                id: uuid::Uuid::new_v4().to_string(),
                payload,
                dedupe_key: digest.clone(),
                priority: 1,
                not_before_ms: now,
                now_ms: now,
                request: JobRequest {
                    scope: "encoded_output_prepare".to_owned(),
                    request_id: digest.clone(),
                    request_digest: digest,
                    consumer_kind: "encoded_output".to_owned(),
                    consumer_ref: file.id.to_string(),
                    target_node_id: Some(node.to_owned()),
                    deadline_ms: None,
                    retain_identity: false,
                },
            })
            .await
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    pub(crate) async fn produce_encoded_output_job(
        &self,
        file: &plurx_core::domain::MediaFile,
        job: &plurx_core::store::background_jobs::BackgroundJob,
        fence: crate::background_jobs::JobFence,
        deadline: Instant,
        observation: u64,
    ) -> Result<bool, crate::background_jobs::PreparationError> {
        use crate::background_jobs::PreparationError;
        use plurx_core::store::background_jobs::JobPayload;
        let payload = job
            .supported_payload()
            .map_err(|_| PreparationError::Fail("encoded_payload_unsupported"))?;
        let JobPayload::EncodedOutputPrepare {
            file_id,
            source_size,
            source_mtime,
            source_object_version,
            policy_generation,
            intent,
            candidate_catalog,
            scratch_bytes,
            ..
        } = payload
        else {
            return Err(PreparationError::Fail("encoded_payload_unsupported"));
        };
        let automatic_candidate = intent.candidate_id.is_some();
        if automatic_candidate && candidate_catalog.is_none()
            || !automatic_candidate && candidate_catalog.is_some()
        {
            return Ok(Self::stop_unverifiable_candidate(&fence).await?);
        }
        if file.id != file_id
            || file.size != source_size
            || file.mtime != source_mtime
            || crate::vodserve::retained::encoded_policy_generation(file, &intent).as_ref()
                != Some(&policy_generation)
        {
            if automatic_candidate {
                return Ok(Self::stop_unverifiable_candidate(&fence).await?);
            }
            return Err(PreparationError::Fail("encoded_policy_changed"));
        }
        if !self.encoded_preparation_still_idle(observation) {
            return Err(PreparationError::Yield("owner_busy"));
        }
        // Match foreground's selected offset without rewriting the stored
        // scanner row. The closed intent owns this delivery fact.
        let mut resolved_file = file.clone();
        resolved_file.audio_offset_ms = if file.audio_streams.is_empty() {
            0
        } else {
            intent.audio_offset_ms
        };
        let file = &resolved_file;
        let mut request = SessionRequest {
            vod_only: false,
            passive_vod: false,
            finite_bitrate_limit_bps: None,
            quality_catalog: None,
            candidate_context: None,
            file_id,
            playback_id: String::new(),
            request_id: None,
            control_sequence: None,
            automatic: false,
            previous_session_id: None,
            reopen_reason: None,
            kind: match intent.copy_for_burn {
                Some((aac, preserve_dolby_vision, convert_dolby_vision)) => SessionKind::Copy {
                    aac,
                    preserve_dolby_vision,
                    convert_dolby_vision,
                },
                None => SessionKind::Transcode {
                    height: i64::from(
                        intent
                            .requested_height
                            .ok_or(PreparationError::Fail("encoded_request_height_unavailable"))?,
                    ),
                },
            },
            start_seconds: 0.0,
            audio_index: intent.audio_index,
            audio_claim: Some(intent.audio_claim.clone()),
            audio_delivery: Some(intent.audio_delivery.clone()),
            subtitle_burn: intent.subtitle_burn,
            audio_offset_ms: intent.audio_offset_ms,
            hdr10: intent.hdr10_requested,
            presentation: Presentation::Vod,
            block_budget_secs: None,
            transport: None,
        };
        let expected = intent.candidate_id.zip(intent.candidate_digest);
        if job.token.as_ref().map(|token| token.node_id.as_str())
            != Some(intent.target_node_id.as_str())
        {
            return Err(PreparationError::Fail("claimed_target_mismatch"));
        }
        if expected.is_some() {
            if !self
                .restore_queued_candidate(
                    &mut request,
                    file,
                    candidate_catalog.as_ref(),
                    expected,
                    &intent.target_node_id,
                    &fence,
                    deadline,
                )
                .await?
            {
                return Ok(false);
            }
        } else if candidate_catalog.is_some() {
            return Ok(Self::stop_unverifiable_candidate(&fence).await?);
        }
        let encoding = self
            .prepare_vod_encoding(&request, file)
            .await?
            .ok_or(PreparationError::Fail("encoded_plan_unavailable"))?;
        if encoding.source_object_version != source_object_version
            || self.encoded_output_intent(&request, &encoding, &intent.target_node_id)? != intent
        {
            if expected.is_some() {
                return Ok(Self::stop_unverifiable_candidate(&fence).await?);
            }
            if encoding.source_object_version != source_object_version {
                return Err(PreparationError::Stop("source_changed"));
            }
            return Err(PreparationError::Fail("encoded_delivery_changed"));
        }
        let settings = self
            .vod_settings(&request)
            .await?
            .ok_or(PreparationError::Fail("vod_policy_unavailable"))?;
        let executable = crate::ffmpeg::EncodedExecutable::capture().await?;
        if executable.digest != intent.executable_digest {
            return Err(PreparationError::Fail("encoded_executable_changed"));
        }
        let engine = encoding.engine.clone();
        let prepared = self
            .vod
            .prepare_encoded_output(
                crate::vodserve::VodRecipeRequest {
                    request: &request,
                    encoding: Some(encoding),
                    retained_capture: crate::vodserve::RetainedOutputCapture::New,
                    measured_candidate: request.candidate_context.as_ref().map(|binding| {
                        crate::vodserve::RetainedCandidateBinding {
                            kind: request.kind,
                            normalized_geometry: binding.normalized_geometry,
                            profile: binding.profile,
                            candidate_id: binding.candidate_id,
                            recipe_digest: binding.recipe_digest,
                            file_id,
                            audio_index: request.audio_index,
                            audio_offset_ms: request.audio_offset_ms,
                            subtitle_burn: request.subtitle_burn,
                            grade: binding.grade,
                            route: plurx_core::playback::candidate::CandidateRoute::Encode,
                        }
                    }),
                },
                file,
                &settings,
                &source_object_version,
                u64::try_from(scratch_bytes)
                    .map_err(|_| PreparationError::Fail("encoded_cap_invalid"))?,
                fence,
                deadline,
                self.admissions.clone(),
                (Arc::new(executable), engine),
                || self.encoded_preparation_still_idle(observation),
            )
            .await?;
        Ok(prepared.settle_encoded_and_expose(&intent).await?)
    }

    #[cfg(test)]
    pub(crate) fn with_copy_test_vod(mut self, vod: Arc<crate::vodserve::VodServe>) -> Self {
        self.vod = vod;
        self
    }
    /// Queue exact complete-copy work without waiting for its full body in
    /// the foreground. This budget is a hard allocation cap, not wire cost.
    pub(crate) async fn enqueue_copy_output(
        &self,
        request: &SessionRequest,
        file: &plurx_core::domain::MediaFile,
        settings: &crate::vodserve::VodSettings,
    ) -> Result<(), String> {
        use plurx_core::store::background_jobs::{
            CopyOutputIntent, CopyOutputProfile, EnqueueJob, JobPayload, JobRequest,
        };
        let SessionKind::Copy {
            aac,
            preserve_dolby_vision,
            convert_dolby_vision,
        } = request.kind
        else {
            return Ok(());
        };
        let context = request.candidate_context.as_ref();
        if context.is_some_and(|context| !context.normalized_geometry) {
            return Ok(());
        }
        let Some(node) = (match context {
            Some(context) => context.owner_node_id.as_deref(),
            None => self.cache_location().map(|(_, node)| node),
        }) else {
            return Ok(());
        };
        let Some(audio_delivery) = request.audio_delivery.clone() else {
            return Ok(());
        };
        if request.subtitle_burn.is_some() || request.audio_claim.is_none() {
            return Ok(());
        }
        if request.audio_offset_ms != file.audio_offset_ms {
            return Ok(());
        }
        let selected_audio = request.audio_index.map_or_else(
            || file.audio_streams.first(),
            |index| {
                file.audio_streams
                    .iter()
                    .find(|stream| stream.index == index)
            },
        );
        let claim = request.audio_claim.as_ref().expect("checked audio claim");
        if !claim.valid_snapshot()
            || audio_delivery
                != plurx_core::playback::audio::resolve_audio(
                    selected_audio,
                    &claim.profile(),
                    plurx_core::playback::audio::AudioRoute::Progressive,
                    file.audio_offset_ms,
                )
        {
            return Ok(());
        }
        let source = crate::fragment_index_cluster::open_source_fence(file, None).await?;
        let probe = self
            .store
            .get_file_probe_json(file.id)
            .await
            .map_err(|error| error.to_string())?;
        let video = plurx_core::transcode::CopyVideoOptions::from_probe(
            file,
            probe.as_deref(),
            crate::ffmpeg::has_dovi_rpu().await,
            preserve_dolby_vision,
        )
        .with_dolby_vision_conversion(convert_dolby_vision);
        let identity = crate::fragindex::identity_for(file, video).argv_fingerprint;
        let executable = crate::ffmpeg::EncodedExecutable::capture().await?;
        let engine = crate::ffmpeg::EncodedEngine::capture(None).await?;
        let width = file
            .width
            .and_then(|value| u32::try_from(value).ok())
            .ok_or("copy width unavailable")?;
        let height = file
            .height
            .and_then(|value| u32::try_from(value).ok())
            .ok_or("copy height unavailable")?;
        let actual = super::manager_candidates::copy_candidate_recipe_digest(
            file,
            request.audio_index,
            request.audio_offset_ms,
            None,
            (aac, preserve_dolby_vision, convert_dolby_vision),
            Some(source.object_version()),
            Some(&executable.digest),
            Some(&engine.digest),
            (width, height),
        );
        if context.is_some_and(|context| {
            actual != context.recipe_digest
                || plurx_core::playback::candidate::CandidateId::for_recipe_digest(actual)
                    != context.candidate_id
                || super::manager_candidates::copy_candidate_grade(file) != context.grade
        }) {
            return Ok(());
        }
        if !source.unchanged() {
            return Ok(());
        }
        let cap = u64::try_from(file.size)
            .ok()
            .and_then(|bytes| bytes.checked_mul(2))
            .and_then(|bytes| bytes.checked_add(64 * 1024 * 1024))
            .map(|bytes| bytes.min(settings.completed_cache_bytes))
            .filter(|bytes| *bytes > 0)
            .ok_or("copy preparation allocation cap unavailable")?;
        let intent = CopyOutputIntent {
            target_node_id: node.to_owned(),
            audio_index: request.audio_index,
            audio_offset_ms: request.audio_offset_ms,
            audio_claim: request.audio_claim.clone(),
            audio_delivery,
            aac,
            preserve_dolby_vision,
            convert_dolby_vision,
            grade: super::manager_candidates::copy_candidate_grade(file),
            hdr10_requested: request.hdr10,
            normalized_geometry: context.is_some(),
            profile: context
                .and_then(|context| context.profile)
                .map(|profile| match profile {
                    plurx_core::transcode::AutoQualityRateProfile::H264Sdr1440P30V1 => {
                        CopyOutputProfile::H264Sdr1440P30V1
                    }
                }),
            width: file
                .width
                .and_then(|width| u32::try_from(width).ok())
                .ok_or("copy width unavailable")?,
            height: file
                .height
                .and_then(|height| u32::try_from(height).ok())
                .ok_or("copy height unavailable")?,
            video_identity: identity,
            pipeline_identity: crate::ffmpeg::fragment_index_engine_digest().await,
        };
        let payload = JobPayload::CopyOutputPrepare {
            copy_output_version: if context.is_some() { 1 } else { 2 },
            file_id: file.id,
            source_generation: source.object_version().to_owned(),
            source_size: file.size,
            source_mtime: file.mtime,
            source_object_version: source.object_version().to_owned(),
            policy_generation: if context.is_some() {
                "copy_output_v1".to_owned()
            } else {
                crate::vodserve::retained::manual_copy_policy_generation(file, &intent)
                    .ok_or("manual source metadata unavailable")?
            },
            intent,
            candidate_catalog: self.queued_candidate_catalog(request, file, node).await?,
            scratch_bytes: i64::try_from(cap).map_err(|_| "copy cap overflow")?,
            reason: "recent_demand".to_owned(),
        };
        let digest = hex::encode(Sha256::digest(
            serde_json::to_vec(&payload).map_err(|error| error.to_string())?,
        ));
        let now = crate::media_sessions::unix_ms();
        self.store
            .enqueue_job(EnqueueJob {
                id: uuid::Uuid::new_v4().to_string(),
                payload,
                dedupe_key: digest.clone(),
                priority: 1,
                not_before_ms: now,
                now_ms: now,
                request: JobRequest {
                    scope: "copy_output_prepare".to_owned(),
                    request_id: digest.clone(),
                    request_digest: digest,
                    consumer_kind: "copy_output".to_owned(),
                    consumer_ref: file.id.to_string(),
                    target_node_id: Some(node.to_owned()),
                    deadline_ms: None,
                    retain_identity: false,
                },
            })
            .await
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    /// Actual copy preparation uses the existing claimed/admitted worker lane
    /// and VOD driver, not a playback session or a second producer scheduler.
    pub(crate) async fn produce_copy_output_job(
        &self,
        file: &plurx_core::domain::MediaFile,
        job: &plurx_core::store::background_jobs::BackgroundJob,
        fence: crate::background_jobs::JobFence,
        admission: &FragmentAdmission,
        deadline: Instant,
    ) -> Result<bool, crate::background_jobs::PreparationError> {
        use crate::background_jobs::PreparationError;
        use plurx_core::store::background_jobs::JobPayload;
        let payload = job
            .supported_payload()
            .map_err(|_| PreparationError::Fail("copy_payload_unsupported"))?;
        let JobPayload::CopyOutputPrepare {
            copy_output_version,
            file_id,
            source_size,
            source_mtime,
            source_object_version,
            policy_generation,
            intent,
            candidate_catalog,
            scratch_bytes,
            ..
        } = &payload
        else {
            return Err(PreparationError::Fail("copy_payload_unsupported"));
        };
        let manual = *copy_output_version == 2;
        if !manual && candidate_catalog.is_none() || manual && candidate_catalog.is_some() {
            return Ok(Self::stop_unverifiable_candidate(&fence).await?);
        }
        if manual
            && *policy_generation
                != crate::vodserve::retained::manual_copy_policy_generation(file, intent)
                    .ok_or(PreparationError::Fail("manual_source_metadata_unavailable"))?
        {
            return Err(PreparationError::Fail(
                "manual_copy_source_metadata_changed",
            ));
        }
        if file.id != *file_id
            || file.size != *source_size
            || file.mtime != *source_mtime
            || !intent.valid()
            || file.width.and_then(|v| u32::try_from(v).ok()) != Some(intent.width)
            || file.height.and_then(|v| u32::try_from(v).ok()) != Some(intent.height)
            || super::manager_candidates::copy_candidate_grade(file) != intent.grade
            || crate::ffmpeg::fragment_index_engine_digest().await != intent.pipeline_identity
        {
            if !manual {
                return Ok(Self::stop_unverifiable_candidate(&fence).await?);
            }
            return Err(PreparationError::Fail("copy_output_facts_changed"));
        }
        if !self.fragment_worker_idle(admission) {
            return Err(PreparationError::Yield("owner_busy"));
        }
        let source = crate::fragment_index_cluster::open_source_fence(file, None).await?;
        if !source.unchanged() || source.object_version() != source_object_version {
            if !manual {
                return Ok(Self::stop_unverifiable_candidate(&fence).await?);
            }
            return Err(PreparationError::Stop("source_changed"));
        }
        let selected_audio = intent.audio_index.map_or_else(
            || file.audio_streams.first(),
            |index| {
                file.audio_streams
                    .iter()
                    .find(|stream| stream.index == index)
            },
        );
        let claim = intent
            .audio_claim
            .as_ref()
            .ok_or(PreparationError::Fail("copy_audio_claim_unavailable"))?;
        let audio = plurx_core::playback::audio::resolve_audio(
            selected_audio,
            &claim.profile(),
            plurx_core::playback::audio::AudioRoute::Progressive,
            intent.audio_offset_ms,
        );
        if audio != intent.audio_delivery {
            if !manual {
                return Ok(Self::stop_unverifiable_candidate(&fence).await?);
            }
            return Err(PreparationError::Fail("copy_audio_delivery_changed"));
        }
        // The stored row has its default offset; this exact claimed intent
        // owns the resolved delivery offset, just as foreground create does.
        let mut resolved_file = file.clone();
        resolved_file.audio_offset_ms = intent.audio_offset_ms;
        let file = &resolved_file;
        let probe = self
            .store
            .get_file_probe_json(file.id)
            .await
            .map_err(|error| error.to_string())?;
        let video = plurx_core::transcode::CopyVideoOptions::from_probe(
            file,
            probe.as_deref(),
            crate::ffmpeg::has_dovi_rpu().await,
            intent.preserve_dolby_vision,
        )
        .with_dolby_vision_conversion(intent.convert_dolby_vision);
        if crate::fragindex::identity_for(file, video).argv_fingerprint != intent.video_identity {
            if !manual {
                return Ok(Self::stop_unverifiable_candidate(&fence).await?);
            }
            return Err(PreparationError::Fail("copy_video_identity_changed"));
        }
        let executable = crate::ffmpeg::EncodedExecutable::capture().await?;
        let engine = crate::ffmpeg::EncodedEngine::capture(None).await?;
        let digest = super::manager_candidates::copy_candidate_recipe_digest(
            file,
            intent.audio_index,
            intent.audio_offset_ms,
            None,
            (
                intent.aac,
                intent.preserve_dolby_vision,
                intent.convert_dolby_vision,
            ),
            Some(source.object_version()),
            Some(&executable.digest),
            Some(&engine.digest),
            (intent.width, intent.height),
        );
        let mut request = SessionRequest {
            vod_only: false,
            passive_vod: false,
            finite_bitrate_limit_bps: None,
            quality_catalog: None,
            candidate_context: None,
            file_id: file.id,
            playback_id: String::new(),
            request_id: None,
            control_sequence: None,
            automatic: false,
            previous_session_id: None,
            reopen_reason: None,
            kind: SessionKind::Copy {
                aac: intent.aac,
                preserve_dolby_vision: intent.preserve_dolby_vision,
                convert_dolby_vision: intent.convert_dolby_vision,
            },
            start_seconds: 0.0,
            audio_index: intent.audio_index,
            audio_claim: Some(claim.clone()),
            audio_delivery: Some(audio),
            subtitle_burn: None,
            audio_offset_ms: intent.audio_offset_ms,
            hdr10: intent.hdr10_requested,
            presentation: Presentation::Vod,
            block_budget_secs: None,
            transport: None,
        };
        if job.token.as_ref().map(|token| token.node_id.as_str())
            != Some(intent.target_node_id.as_str())
        {
            return Err(PreparationError::Fail("claimed_target_mismatch"));
        }
        if !manual {
            let id = plurx_core::playback::candidate::CandidateId::for_recipe_digest(digest);
            if !self
                .restore_queued_candidate(
                    &mut request,
                    file,
                    candidate_catalog.as_ref(),
                    Some((id, digest)),
                    &intent.target_node_id,
                    &fence,
                    deadline,
                )
                .await?
            {
                return Ok(false);
            }
            let context = request
                .candidate_context
                .as_ref()
                .expect("restored context");
            let profile = context.profile.map(|profile| match profile {
                plurx_core::transcode::AutoQualityRateProfile::H264Sdr1440P30V1 => {
                    plurx_core::store::background_jobs::CopyOutputProfile::H264Sdr1440P30V1
                }
            });
            if context.grade != intent.grade
                || context.normalized_geometry != intent.normalized_geometry
                || profile != intent.profile
                || context.selected_candidate.width != intent.width
                || context.selected_candidate.height != intent.height
            {
                return Ok(Self::stop_unverifiable_candidate(&fence).await?);
            }
        } else if candidate_catalog.is_some() {
            return Ok(Self::stop_unverifiable_candidate(&fence).await?);
        }
        let settings = self
            .vod_settings(&request)
            .await?
            .ok_or(PreparationError::Fail("vod_policy_unavailable"))?;
        let prepared_request = crate::vodserve::VodRecipeRequest {
            request: &request,
            encoding: None,
            retained_capture: crate::vodserve::RetainedOutputCapture::New,
            measured_candidate: request.candidate_context.as_ref().map(|binding| {
                crate::vodserve::RetainedCandidateBinding {
                    kind: request.kind,
                    normalized_geometry: binding.normalized_geometry,
                    profile: binding.profile,
                    candidate_id: binding.candidate_id,
                    recipe_digest: binding.recipe_digest,
                    file_id: file.id,
                    audio_index: request.audio_index,
                    audio_offset_ms: request.audio_offset_ms,
                    subtitle_burn: None,
                    grade: binding.grade,
                    route: plurx_core::playback::candidate::CandidateRoute::Remux,
                }
            }),
        };
        let prepared = self
            .vod
            .prepare_copy_output(
                prepared_request,
                file,
                &settings,
                source_object_version,
                u64::try_from(*scratch_bytes)
                    .map_err(|_| PreparationError::Fail("copy_cap_invalid"))?,
                fence,
                deadline,
                self.admissions.clone(),
                (Arc::new(executable), engine),
                || self.fragment_worker_idle(admission),
            )
            .await?;
        if !source.unchanged() {
            return Err(PreparationError::Stop("source_changed"));
        }
        if !self.fragment_worker_idle(admission) {
            return Err(PreparationError::Yield("preempted"));
        }
        Ok(prepared.settle_and_expose(intent).await?)
    }

    /// Pre-transcode one file at one rung, so the next viewer gets a cache hit.
    ///
    /// Runs at background priority and is expected to be interrupted: an encode
    /// of a two-hour 4K film will normally be preempted several times by people
    /// pressing play, and picks up from its last published segment boundary
    /// each time — within this call, and across later ones. Preemption is by
    /// *termination* — see [`crate::admission`] for why suspending would not
    /// release anything that matters.
    ///
    /// **One of these at a time per node.** Queue claiming and the worker's
    /// process-local guard enforce that invariant for production traffic.
    ///
    /// Returns the recipe hash on success, `None` when there was nothing to do
    /// (already cached, already claimed by another producer, no cache
    /// configured) — neither of which is a failure.
    #[cfg(test)]
    pub async fn produce(
        &self,
        file: &plurx_core::domain::MediaFile,
        target_height: i64,
        deadline: Instant,
    ) -> Result<Option<Produced>, String> {
        let cancelled = tokio_util::sync::CancellationToken::new();
        Ok(
            match self
                .produce_attempt(
                    file,
                    target_height,
                    deadline,
                    &cancelled,
                    None,
                    None,
                    None,
                    None,
                )
                .await?
            {
                PretranscodeProduceOutcome::Ready(produced) => Some(produced),
                PretranscodeProduceOutcome::Yielded(_)
                | PretranscodeProduceOutcome::StoreUnavailable
                | PretranscodeProduceOutcome::PolicyChanged
                | PretranscodeProduceOutcome::SourceChanged
                // Speculative warming has nothing to settle: it holds no queue
                // row and no package. The bytes were served if anyone was
                // waiting and are gone; there is nothing to report but the
                // absence of a warmed entry.
                | PretranscodeProduceOutcome::HealthRefused => None,
            },
        )
    }

    /// Execute one claimed queue row through the same producer and admission
    /// lane as live/offline work.
    pub async fn produce_pretranscode_job(
        &self,
        file: &plurx_core::domain::MediaFile,
        target_height: i64,
        deadline: Instant,
        cancelled: &tokio_util::sync::CancellationToken,
        source: BoundPretranscodeSource,
        fence: PretranscodeFence,
    ) -> Result<PretranscodeProduceOutcome, String> {
        let snapshot = source.snapshot;
        self.produce_attempt(
            file,
            target_height,
            deadline,
            cancelled,
            Some(snapshot),
            Some(Arc::new(source)),
            None,
            Some(fence),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn produce_attempt(
        &self,
        file: &plurx_core::domain::MediaFile,
        target_height: i64,
        deadline: Instant,
        cancelled: &tokio_util::sync::CancellationToken,
        expected_source_snapshot: Option<LocalSourceSnapshot>,
        bound_source: Option<Arc<BoundPretranscodeSource>>,
        publication_fence: Option<PublicationFence>,
        pretranscode_fence: Option<PretranscodeFence>,
    ) -> Result<PretranscodeProduceOutcome, String> {
        if cancelled.is_cancelled() {
            return Ok(PretranscodeProduceOutcome::Yielded("ownership_lost"));
        }
        if self.cache.is_none() {
            return Ok(PretranscodeProduceOutcome::Yielded("cache_unavailable"));
        }
        if self
            .offline_waiting
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Ok(PretranscodeProduceOutcome::Yielded("offline_waiting"));
        }
        let _producer = match self.background_producer.try_lock() {
            Ok(permit) => permit,
            Err(_) => return Ok(PretranscodeProduceOutcome::Yielded("producer_busy")),
        };
        let policy = match self.try_pretranscode_policy_snapshot().await {
            Ok(policy) => policy,
            Err(error) => {
                tracing::warn!(
                    target: "plurxd::transcode",
                    %error, "speculative worker could not read transcode policy"
                );
                return Ok(PretranscodeProduceOutcome::Yielded("policy_unavailable"));
            }
        };
        if let Some(fence) = &pretranscode_fence {
            let Some(job) = fence.snapshot().await else {
                return Ok(PretranscodeProduceOutcome::Yielded("ownership_lost"));
            };
            if job.policy_generation != policy.generation {
                return Ok(PretranscodeProduceOutcome::PolicyChanged);
            }
        }
        // A background artifact is the zero-offset default shared by future
        // plays. Never bake a historical, file-persisted correction into it.
        let mut playback_file = file.clone();
        playback_file.audio_offset_ms = 0;
        let file = &playback_file;
        let encoder = self
            .encoder_for_file_with_preference(
                file,
                &policy.requested_encoder,
                crate::process_control::ChildClass::Background,
            )
            .await?;
        if pretranscode_fence
            .as_ref()
            .is_some_and(|fence| fence.admission.encoder != encoder && encoder != Encoder::Software)
        {
            return Ok(PretranscodeProduceOutcome::PolicyChanged);
        }
        if !policy
            .acceptable_encoder_families()
            .iter()
            .any(|family| family == encoder.family_name())
        {
            return Ok(PretranscodeProduceOutcome::Yielded("encoder_not_allowed"));
        }
        // Through the same track selection a real playback uses. Not an
        // optimisation — the tracks are part of the recipe, so producing with
        // "no audio track chosen" makes an entry named for a session that will
        // never be requested.
        let Tracks {
            audio_index,
            subtitle_burn,
        } = Self::select_tracks_with_prefs(
            file,
            None,
            None,
            &policy.prefs,
            // A speculative artifact is an SDR ladder rung: nothing here asks
            // for the HDR10 grade, so `hdr10_grade_for` answers `Sdr` on its
            // second line and there is no grade to lose. Asking `grade_preview`
            // would be a store read and, on a Profile 5 source with a cold
            // proof cache, an ffmpeg pass — to compute a constant.
            false,
        );
        let mut opts = self.speculative_producer_options(
            policy.rate_control,
            encoder,
            file,
            target_height,
            audio_index,
            subtitle_burn,
        );
        opts.software_threads = pretranscode_fence
            .as_ref()
            .map(|fence| fence.admission.threads as u32)
            .or_else(|| Some(Workload::of(file, target_height).software_threads() as u32));
        if let Some(mode) = self
            .analyze_content_for_producer(
                file,
                &opts,
                encoder,
                bound_source.as_ref(),
                pretranscode_fence.as_ref(),
                cancelled,
                deadline,
            )
            .await
        {
            opts.effective_rate_control = mode;
        }
        // Bind decoder facts and the immutable plan before deriving any cache,
        // singleflight, staging, or publication identity. The held descriptor
        // used here is the same source descriptor later inherited by ffmpeg.
        let planning_started = Instant::now();
        let plan = match bound_source.as_ref() {
            Some(source) => {
                self.resolve_bound_movie_plan(
                    file,
                    &opts,
                    encoder,
                    source,
                    deadline,
                    Some(cancelled),
                )
                .await?
            }
            None => self.resolve_movie_plan(file, &opts, encoder).await?,
        };
        let deadline =
            retain_production_budget_after_planning(deadline, planning_started.elapsed());
        let digest = self.digest().ok_or("no cache digest")?;
        let hash = self.effective_recipe(&digest, &plan, false).hash();
        if cancelled.is_cancelled() {
            return Ok(PretranscodeProduceOutcome::Yielded("ownership_lost"));
        }
        if let Some(fence) = &pretranscode_fence {
            if !fence
                .durable
                .bind_transcode_recipe(&hash)
                .await
                .map_err(|error| error.to_string())?
            {
                return Ok(PretranscodeProduceOutcome::Yielded(
                    "recipe_binding_refused",
                ));
            }
        }
        let queue_owned = pretranscode_fence.is_some();

        Ok(
            match self
                .produce_normalized(
                    PortableProduction {
                        file,
                        opts: &opts,
                        plan: &plan,
                        deadline,
                        yield_to_offline: true,
                        cancelled: Some(cancelled),
                        offline_package_id: None,
                        offline_claim_generation: None,
                        publication_fence,
                        pretranscode_fence,
                        expected_policy_generation: queue_owned
                            .then_some(policy.generation.clone()),
                        expected_source_snapshot,
                        bound_source,
                    },
                    hash,
                )
                .await?
            {
                OfflineProduceOutcome::Ready(produced) => {
                    PretranscodeProduceOutcome::Ready(produced)
                }
                OfflineProduceOutcome::Cached(produced) if queue_owned => {
                    PretranscodeProduceOutcome::Ready(produced)
                }
                OfflineProduceOutcome::PolicyChanged => PretranscodeProduceOutcome::PolicyChanged,
                OfflineProduceOutcome::SourceChanged => PretranscodeProduceOutcome::SourceChanged,
                OfflineProduceOutcome::StoreUnavailable => {
                    PretranscodeProduceOutcome::StoreUnavailable
                }
                OfflineProduceOutcome::HealthRefused => PretranscodeProduceOutcome::HealthRefused,
                OfflineProduceOutcome::Yielded(reason) => {
                    PretranscodeProduceOutcome::Yielded(reason)
                }
                OfflineProduceOutcome::Cached(_) | OfflineProduceOutcome::ClaimedElsewhere => {
                    PretranscodeProduceOutcome::Yielded("claimed_elsewhere")
                }
            },
        )
    }

    /// The node id a package produced here is owned by, for the fenced
    /// package writes. Offline production requires a cache, so a manager
    /// without one owns no package and its fenced writes correctly match
    /// nothing.
    fn offline_owner(&self) -> &str {
        self.cache
            .as_ref()
            .map_or("", |cache| cache.node_id.as_str())
    }

    /// Join only a currently resolved exact recipe with local delivery. This
    /// waits without holding an encoder or asking the matching producer to yield.
    async fn wait_for_shared_offline_preparation(
        &self,
        package: &OfflinePackage,
        recipe_hash: &str,
        deadline: Instant,
        cancelled: &tokio_util::sync::CancellationToken,
    ) -> Result<bool, String> {
        use plurx_core::store::background_jobs::{EnqueueOutcome, JobState, JoinOfflineJob};
        let joined = self
            .store
            .join_offline_job(JoinOfflineJob {
                package_id: package.id.clone(),
                node_id: package.node_id.clone(),
                claim_generation: package.claim_generation,
                recipe_hash: recipe_hash.into(),
                now_ms: unix_ms(),
            })
            .await
            .map_err(|error| error.to_string())?;
        let job_id = match joined {
            Some(
                EnqueueOutcome::Accepted { job_id, .. }
                | EnqueueOutcome::Existing {
                    job_id,
                    cancelled: false,
                    ..
                },
            ) => job_id,
            _ => return Ok(true),
        };
        let result = async {
        loop {
            if cancelled.is_cancelled() || Instant::now() >= deadline {
                return Ok(false);
            }
            if !self
                .store
                .offline_package_claim_is_current(
                    &package.id,
                    &package.node_id,
                    package.claim_generation,
                    recipe_hash,
                )
                .await
                .map_err(|error| error.to_string())?
            {
                return Ok(false);
            }
            let Some(job) = self
                .store
                .background_job(&job_id)
                .await
                .map_err(|error| error.to_string())?
            else {
                return Ok(false);
            };
            match job.state {
                JobState::Succeeded => return Ok(true), // The normal path validates the local manifest next.
                JobState::Failed => {
                    return Err(
                        "shared preparation exhausted its retry budget or failed validation".into(),
                    )
                }
                JobState::Cancelled | JobState::Cancelling => return Ok(false),
                JobState::Queued | JobState::Running => {}
            }
            tokio::select! {
                () = cancelled.cancelled() => return Ok(false),
                () = tokio::time::sleep_until(deadline.min(Instant::now() + Duration::from_secs(3)).into()) => {},
            }
        }
        }.await;
        // A timed-out caller must not leave priority or local affinity behind.
        // Package mutation also retires this receipt atomically if the caller dies.
        if !matches!(result, Ok(true)) {
            self.store
                .cancel_waiter(plurx_core::store::background_jobs::CancelWaiter {
                    scope: format!("user:{}", package.user_id),
                    request_id: format!("{}:{}", package.id, package.claim_generation),
                    now_ms: unix_ms(),
                })
                .await
                .map_err(|error| error.to_string())?;
        }
        result
    }

    async fn offline_decode_alternate(
        &self,
        package_id: &str,
        file: &plurx_core::domain::MediaFile,
        opts: &TranscodeOptions,
        primary_plan: &ResolvedTranscode,
        encoder: Encoder,
        digest: &PipelineDigest,
    ) -> Option<(TranscodeOptions, ResolvedTranscode, String)> {
        let alternate_plan = match self
            .resolve_restricted_movie_plan(
                file,
                opts,
                encoder,
                &AttemptRestrictions::requiring(plurx_core::transcode::DecodeBackend::Software),
            )
            .await
        {
            Ok(plan) => plan,
            Err(error) => {
                tracing::warn!(target: "plurxd::transcode", package = package_id, %error, "offline decode alternate did not resolve");
                return None;
            }
        };
        match decode_restricted_options(primary_plan, &alternate_plan, opts) {
            Ok(Some(alternate_opts)) => {
                let hash = self.effective_recipe(digest, &alternate_plan, false).hash();
                Some((alternate_opts, alternate_plan, hash))
            }
            Ok(None) => None,
            Err(error) => {
                tracing::warn!(
                    target: "plurxd::transcode",
                    package = package_id, %error, "offline decode alternate is unsafe"
                );
                None
            }
        }
    }

    /// Prepare the exact mobile package requested by an authenticated user.
    /// Unlike speculative production, this preserves the file's A/V offset,
    /// accepts explicit tracks, and forces SDR even on a passthrough node.
    pub async fn ensure_offline(
        &self,
        package: &OfflinePackage,
        file: &plurx_core::domain::MediaFile,
        spec: &OfflineSpec,
        deadline: Instant,
        cancelled: &tokio_util::sync::CancellationToken,
    ) -> Result<OfflineProduceOutcome, String> {
        if cancelled.is_cancelled() {
            return Ok(OfflineProduceOutcome::Yielded("ownership_lost"));
        }
        let encoder = self
            .encoder_for_file(file, crate::process_control::ChildClass::Background)
            .await?;
        let subtitle_burn = match spec.subtitle {
            OfflineSubtitle::Burn(index) => {
                let stream = file
                    .subtitle_streams
                    .iter()
                    .find(|stream| stream.index == index)
                    .ok_or_else(|| "offline subtitle track disappeared".to_owned())?;
                Some(plurx_core::transcode::SubtitleBurn {
                    subtitle_index: index,
                    bitmap: plurx_core::tracks::is_bitmap_subtitle(&stream.codec),
                })
            }
            OfflineSubtitle::None | OfflineSubtitle::Native(_) => None,
        };
        let opts = self.offline_package_options(encoder, file, spec, subtitle_burn);
        let plan = self.resolve_movie_plan(file, &opts, encoder).await?;
        let digest = self.digest().ok_or("no cache digest")?;
        let primary_hash = self.effective_recipe(&digest, &plan, false).hash();
        let mut recovery_state = OfflineRecoveryState::parse(&package.decoder_recovery_state)?;
        if recovery_state == OfflineRecoveryState::Primary {
            if package.alternate_recipe_hash.is_some()
                || package
                    .recipe_hash
                    .as_deref()
                    .is_some_and(|stored| stored != primary_hash)
            {
                return Err("primary offline recovery state carries a mismatched recipe".to_owned());
            }

            if !self
                .store
                .set_offline_package_recipe(
                    &package.id,
                    &package.node_id,
                    package.claim_generation,
                    &primary_hash,
                )
                .await
                .map_err(|error| error.to_string())?
            {
                return Ok(OfflineProduceOutcome::Yielded("recipe_binding_refused"));
            }
            if !self
                .wait_for_shared_offline_preparation(package, &primary_hash, deadline, cancelled)
                .await?
            {
                return Ok(OfflineProduceOutcome::Yielded(
                    "shared_preparation_interrupted",
                ));
            }
        }
        struct Waiting<'a>(&'a AtomicBool);
        impl Drop for Waiting<'_> {
            fn drop(&mut self) {
                self.0.store(false, std::sync::atomic::Ordering::Release);
            }
        }
        self.offline_waiting
            .store(true, std::sync::atomic::Ordering::Release);
        let waiting = Waiting(&self.offline_waiting);
        let _producer = self.background_producer.lock().await;
        drop(waiting);
        if cancelled.is_cancelled() {
            return Ok(OfflineProduceOutcome::Yielded("ownership_lost"));
        }
        // Offline requests retain their accepted recipe. Their durable worker
        // can still prepare measured evidence for subsequent requests without
        // changing this package's immutable rate-control snapshot.
        if self.content_encoding_enabled().await {
            if let Ok(libraries) = self.store.list_libraries().await {
                let roots = libraries
                    .into_iter()
                    .flat_map(|library| library.paths)
                    .collect::<Vec<_>>();
                if let Some(source) = pretranscode_source_snapshot(file, &roots).await {
                    let source = Arc::new(source);
                    let _ = self
                        .analyze_content_for_producer(
                            file,
                            &opts,
                            encoder,
                            Some(&source),
                            None,
                            cancelled,
                            deadline,
                        )
                        .await;
                }
            }
        }
        let mut recovery_began_now = false;
        let mut outcome = OfflineProduceOutcome::HealthRefused;
        if recovery_state == OfflineRecoveryState::Primary {
            outcome = self
                .produce_offline_candidate(
                    package,
                    file,
                    &opts,
                    &plan,
                    &primary_hash,
                    deadline,
                    cancelled,
                )
                .await?;
            if matches!(outcome, OfflineProduceOutcome::HealthRefused) {
                // Software is already the terminal safe route. The direct
                // Developer switch, not artifact qualification, authorizes a
                // new automatic recovery; neither refusal may mint a consumed
                // budget that a later node could mistake for a failed
                // hardware-primary attempt.
                if plan.decode().backend() == plurx_core::transcode::DecodeBackend::Software
                    || !self.automatic_decoder_recovery_enabled()
                {
                    return Ok(OfflineProduceOutcome::HealthRefused);
                }
                // The terminal fault consumes the budget *before* alternate
                // planning. A crash, cancellation, or store outage from this
                // point can only resume pending recovery; it cannot run the
                // failed primary again.
                let recovery_begin = match self.hooks.get().offline_recovery_begin_fault() {
                    Some(error) => Err(error),
                    None => {
                        self.store
                            .begin_offline_decode_recovery(
                                &package.id,
                                &package.node_id,
                                package.claim_generation,
                                &primary_hash,
                            )
                            .await
                    }
                };
                let consumed = match recovery_begin {
                    Ok(consumed) => consumed,
                    Err(error) => {
                        tracing::warn!(target: "plurxd::transcode", package = %package.id, %error, "offline recovery budget could not be consumed");
                        // Retrying primary without authoritative proof that
                        // the one-shot budget was consumed is unsafe. Settle
                        // this attempt as terminal; the package coordinator
                        // records decode_unhealthy instead of requeueing the
                        // failed decoder.
                        return Ok(OfflineProduceOutcome::HealthRefused);
                    }
                };
                if !consumed || cancelled.is_cancelled() {
                    return Ok(OfflineProduceOutcome::Yielded("recovery_interrupted"));
                }
                recovery_state = OfflineRecoveryState::Pending;
                recovery_began_now = true;
                tracing::warn!(
                    target: "plurxd::transcode",
                    package = %package.id,
                    failed_recipe = primary_hash,
                    "offline decode fault consumed the durable recovery budget"
                );
            }
        }
        if recovery_state != OfflineRecoveryState::Primary {
            // A consumed job re-homed from a hardware owner may land on a
            // software-only survivor. On that node the ordinary plan is
            // already the safe route, so restricting it to software produces
            // the same digest and the ordinary alternate helper correctly
            // answers None. `rehome_pending` is the durable proof that this is
            // not a same-owner retry of a failed software primary: the old
            // owner had already consumed the budget before removal, and the
            // removal transaction cleared any owner-local identity without
            // restoring the budget to primary.
            let survivor_local_software = recovery_state == OfflineRecoveryState::RehomePending
                || (recovery_state == OfflineRecoveryState::Alternate
                    && package.alternate_recipe_hash.as_deref() == Some(primary_hash.as_str()));
            let alternate = if survivor_local_software
                && plan.decode().backend() == plurx_core::transcode::DecodeBackend::Software
            {
                Some((opts.clone(), plan.clone(), primary_hash.clone()))
            } else {
                self.offline_decode_alternate(&package.id, file, &opts, &plan, encoder, &digest)
                    .await
            };
            let Some((alternate_opts, alternate_plan, alternate_hash)) = alternate else {
                return Ok(OfflineProduceOutcome::HealthRefused);
            };
            match recovery_state {
                OfflineRecoveryState::Primary => unreachable!("handled above"),
                OfflineRecoveryState::Pending | OfflineRecoveryState::RehomePending => {
                    if !recovery_began_now
                        && (package.recipe_hash.is_some()
                            || package.alternate_recipe_hash.is_some())
                    {
                        return Err(
                            "pending offline recovery already carries an alternate recipe"
                                .to_owned(),
                        );
                    }
                    let installed = match self
                        .store
                        .install_offline_decode_alternate(
                            &package.id,
                            &package.node_id,
                            package.claim_generation,
                            &alternate_hash,
                        )
                        .await
                    {
                        Ok(installed) => installed,
                        Err(error) => {
                            tracing::warn!(target: "plurxd::transcode", package = %package.id, %error, "offline recovery alternate could not be installed");
                            return Ok(OfflineProduceOutcome::StoreUnavailable);
                        }
                    };
                    if !installed || cancelled.is_cancelled() {
                        return Ok(OfflineProduceOutcome::Yielded("recipe_binding_refused"));
                    }
                }
                OfflineRecoveryState::Alternate => {
                    if package.alternate_recipe_hash.as_deref() != Some(alternate_hash.as_str())
                        || package
                            .recipe_hash
                            .as_deref()
                            .is_some_and(|stored| stored != alternate_hash)
                    {
                        return Err(
                            "stored offline alternate does not match the frozen plan".to_owned()
                        );
                    }
                    if !self
                        .store
                        .set_offline_package_recipe(
                            &package.id,
                            &package.node_id,
                            package.claim_generation,
                            &alternate_hash,
                        )
                        .await
                        .map_err(|error| error.to_string())?
                    {
                        return Ok(OfflineProduceOutcome::Yielded("recipe_binding_refused"));
                    }
                }
            }
            outcome = self
                .produce_offline_candidate(
                    package,
                    file,
                    &alternate_opts,
                    &alternate_plan,
                    &alternate_hash,
                    deadline,
                    cancelled,
                )
                .await?;
        }
        if matches!(
            outcome,
            OfflineProduceOutcome::Ready(_) | OfflineProduceOutcome::Cached(_)
        ) {
            if let OfflineSubtitle::Native(index) = spec.subtitle {
                // Best effort: package production continues and its later
                // terminal settlement supersedes this progress snapshot.
                crate::store_result::observe(
                    crate::store_result::Operation::UpdateOfflineProgressExtractingSubtitles,
                    crate::store_result::Discard::BestEffort,
                    self.store
                        .update_offline_progress(
                            &package.id,
                            &package.node_id,
                            package.claim_generation,
                            "extracting_subtitles",
                            999,
                        )
                        .await,
                );
                let stored = self.subtitle_source_access();
                crate::subtitles::ensure_vtt_with_store(
                    &self.subtitle_cache,
                    file,
                    index,
                    &stored,
                    crate::process_control::ChildWork::background(
                        "subtitle track for an offline package",
                    ),
                )
                .await?;
            }
        }
        Ok(outcome)
    }

    #[allow(clippy::too_many_arguments)]
    async fn produce_offline_candidate(
        &self,
        package: &OfflinePackage,
        file: &plurx_core::domain::MediaFile,
        opts: &TranscodeOptions,
        plan: &ResolvedTranscode,
        hash: &str,
        deadline: Instant,
        cancelled: &tokio_util::sync::CancellationToken,
    ) -> Result<OfflineProduceOutcome, String> {
        if let Some(outcome) = self.hooks.get().scripted_offline_outcome(hash) {
            return Ok(outcome);
        }
        self.produce_normalized(
            PortableProduction {
                file,
                opts,
                plan,
                deadline,
                yield_to_offline: false,
                cancelled: Some(cancelled),
                offline_package_id: Some(&package.id),
                offline_claim_generation: Some(package.claim_generation),
                publication_fence: None,
                pretranscode_fence: None,
                expected_policy_generation: None,
                expected_source_snapshot: None,
                bound_source: None,
            },
            hash.to_owned(),
        )
        .await
    }

    /// Shared content-addressed production tail. Track and eligibility policy
    /// live above this point; claiming, resume, publication, and cache identity
    /// live here once for scheduled and requested work.
    async fn produce_normalized(
        &self,
        request: PortableProduction<'_>,
        hash: String,
    ) -> Result<OfflineProduceOutcome, String> {
        let PortableProduction {
            file,
            opts,
            plan,
            deadline,
            yield_to_offline: _,
            cancelled,
            offline_package_id,
            offline_claim_generation,
            publication_fence,
            pretranscode_fence,
            expected_policy_generation,
            expected_source_snapshot,
            bound_source,
        } = &request;
        let cancelled = *cancelled;
        let offline_package_id = *offline_package_id;
        let offline_claim_generation = *offline_claim_generation;
        let pretranscode_fence = pretranscode_fence.clone();
        let cache = self.cache.as_ref().ok_or("no cache configured")?;
        let queue_job = if let Some(fence) = &pretranscode_fence {
            let Some(job) = fence.snapshot().await else {
                return Ok(OfflineProduceOutcome::Yielded("ownership_lost"));
            };
            Some(job)
        } else {
            None
        };
        if cancelled.is_some_and(tokio_util::sync::CancellationToken::is_cancelled) {
            return Ok(OfflineProduceOutcome::Yielded("ownership_lost"));
        }
        // Queue reuse has the same lookup/delete race as playback reuse. Hold
        // the recipe guard through manifest validation and fenced completion,
        // otherwise eviction can remove the bytes and row before the queue
        // transaction re-publishes that location as ready.
        let Some(cache_lookup) = self.cache_readers.begin_lookup(&hash) else {
            return Ok(OfflineProduceOutcome::Yielded("cache_maintenance"));
        };
        let cached = match self.store.cache_hit(&hash, &cache.node_id).await {
            Ok(cached) => cached,
            Err(error) => {
                tracing::warn!(target: "plurxd::transcode", recipe = %hash, %error, "cache lookup unavailable during production");
                return Ok(OfflineProduceOutcome::StoreUnavailable);
            }
        };
        if let Some(cached) = cached {
            let Some(_cache_reader) = self.cache_readers.begin_read(&hash) else {
                return Ok(OfflineProduceOutcome::Yielded("cache_maintenance"));
            };
            drop(cache_lookup);
            let cache_location = CachedLocationIdentity {
                recipe_hash: hash.clone(),
                node_id: cache.node_id.clone(),
                storage_class: cached.storage_class.clone(),
                generation_id: None,
                relative_dir: cached.relative_dir.clone(),
                manifest_digest: cached.manifest_digest.clone(),
            };
            if cancelled.is_some_and(tokio_util::sync::CancellationToken::is_cancelled) {
                return Ok(OfflineProduceOutcome::Yielded("ownership_lost"));
            }
            let Some(root) =
                crate::cachekeep::validated_entry_dir(&cache.dir, &cached.relative_dir).await
            else {
                self.invalidate_cache_location(&cache_location, "unsafe_relative_path")
                    .await;
                return Err("cache row contains an unsafe relative path".to_owned());
            };
            let mut manifest = if let Some(expected) = cached.manifest_digest.as_deref() {
                let manifest_path = root.join(plurx_core::transcode::manifest::MANIFEST_FILE);
                if tokio::fs::metadata(&manifest_path).await.is_err() {
                    self.invalidate_cache_location(&cache_location, "manifest_missing")
                        .await;
                    return Err("cached generation lost its fenced manifest".to_owned());
                }
                match crate::manifest_cache::load(
                    crate::manifest_cache::GenerationKey {
                        cache_root: cache.dir.clone(),
                        node_id: cache.node_id.clone(),
                        recipe_hash: hash.clone(),
                        storage_class: cached.storage_class.clone(),
                        relative_dir: cached.relative_dir.clone(),
                        manifest_digest: expected.to_owned(),
                    },
                    &root,
                )
                .await
                {
                    Ok(manifest) => Some(manifest),
                    Err(error) => {
                        self.invalidate_cache_location(&cache_location, "manifest_invalid")
                            .await;
                        return Err(format!("cached generation manifest is invalid: {error}"));
                    }
                }
            } else {
                None
            };
            if !generation_permits_reuse(plan, manifest.as_deref()) {
                // Unreachable while the retention rule below holds, because a
                // row under this identity is only ever written for a
                // generation whose receipt permitted it, and the namespace
                // names the receipt version so a build that cannot read one
                // never computes this key. Reaching it means that invariant is
                // broken.
                //
                // Terminal rather than `Err`, and that distinction is the
                // whole point: `Err` is retryable in both callers, so a
                // condition stored on disk would be retried on a backoff
                // forever and reported to a user as an encoder fault. It also
                // does not invalidate — deleting bytes and failing dependent
                // packages over a bookkeeping fault is the trade this rule
                // exists to refuse.
                tracing::error!(
                    target: "plurxd::transcode",
                    recipe = %hash,
                    namespace = plan.artifact_namespace(),
                    "a health-qualified cache row has no receipt permitting its reuse"
                );
                return Ok(OfflineProduceOutcome::HealthRefused);
            }
            let playlist_bytes = match &manifest {
                Some(manifest) => manifest
                    .read_verified_playlist(&root, "index.m3u8")
                    .await
                    .map_err(|error| format!("verifying cached playlist: {error}"))?,
                None => plurx_core::transcode::manifest::read_bounded_playlist(&root, "index.m3u8")
                    .await
                    .map_err(|error| format!("reading cached playlist: {error}"))?,
            };
            let Some(playlist_bytes) = playlist_bytes else {
                self.invalidate_cache_location(&cache_location, "playlist_object_mismatch")
                    .await;
                return Err("cached playlist failed bounded generation verification".to_owned());
            };
            let playlist = match String::from_utf8(playlist_bytes) {
                Ok(playlist) => playlist,
                Err(error) => {
                    self.invalidate_cache_location(&cache_location, "playlist_invalid_utf8")
                        .await;
                    return Err(format!("cached playlist is not valid UTF-8: {error}"));
                }
            };
            let Some(part) = validated_vod_part(&playlist) else {
                self.invalidate_cache_location(&cache_location, "playlist_invalid_vod")
                    .await;
                return Err("complete cache row contains an invalid VOD playlist".to_owned());
            };
            let mut settled_bytes = cached.bytes;
            if let Some(package_id) = offline_package_id {
                let claim_generation = offline_claim_generation
                    .ok_or("offline package production lost its claim generation")?;
                // Best effort: this near-complete progress row is advisory;
                // final package settlement is the durable result.
                crate::store_result::observe(
                    crate::store_result::Operation::UpdateOfflineProgressCachedTranscode,
                    crate::store_result::Discard::BestEffort,
                    self.store
                        .update_offline_progress(
                            package_id,
                            &cache.node_id,
                            claim_generation,
                            "transcoding",
                            999,
                        )
                        .await,
                );
            }
            if let Some(fence) = &pretranscode_fence {
                if let Some(expected) = expected_policy_generation.as_deref() {
                    if let Some(outcome) = self.pretranscode_policy_interruption(expected).await {
                        return Ok(outcome);
                    }
                }
                if let Some(expected) = expected_source_snapshot {
                    if bound_source_snapshot(bound_source.as_deref()).await != Some(*expected) {
                        return Ok(OfflineProduceOutcome::SourceChanged);
                    }
                }
                let generation_id = queue_job
                    .as_ref()
                    .map(|job| format!("{}:{}", job.id, job.fence))
                    .ok_or("queue publication lost its generation")?;
                let names = std::iter::once("index.m3u8".to_owned())
                    .chain(part.segments.iter().cloned())
                    .collect::<Vec<_>>();
                let adopting_legacy = cached.manifest_digest.is_none();
                if manifest.is_none() {
                    let mut manifest_yield_reason = None;
                    manifest = Some(std::sync::Arc::new(
                        match plurx_core::transcode::manifest::publish_controlled(
                            &root,
                            &generation_id,
                            &names,
                            // Adopting a generation an earlier build published.
                            // This run watched none of those bytes being made,
                            // and a receipt is a claim about bytes you watched.
                            None,
                            || {
                                manifest_yield_reason = if cancelled
                                    .is_some_and(tokio_util::sync::CancellationToken::is_cancelled)
                                {
                                    Some("ownership_lost")
                                } else if Instant::now() >= *deadline {
                                    Some("production_deadline")
                                } else {
                                    self.pretranscode_publication_yield_reason()
                                };
                                manifest_yield_reason.is_some()
                            },
                        )
                        .await
                        {
                            Ok(Some(manifest)) => manifest,
                            Ok(None) => {
                                return Ok(OfflineProduceOutcome::Yielded(
                                    manifest_yield_reason.unwrap_or("manifest_interrupted"),
                                ))
                            }
                            Err(error) => {
                                self.invalidate_cache_location(
                                    &cache_location,
                                    "legacy_manifest_adoption_failed",
                                )
                                .await;
                                return Err(error);
                            }
                        },
                    ));
                }
                let manifest_bytes = if adopting_legacy {
                    tokio::fs::metadata(root.join(plurx_core::transcode::manifest::MANIFEST_FILE))
                        .await
                        .map_err(|error| format!("measuring adopted manifest: {error}"))?
                        .len()
                        .min(i64::MAX as u64) as i64
                } else {
                    0
                };
                settled_bytes = cached.bytes.saturating_add(manifest_bytes);
                if let Some(expected) = expected_policy_generation.as_deref() {
                    if let Some(outcome) = self.pretranscode_policy_interruption(expected).await {
                        return Ok(outcome);
                    }
                }
                if let Some(expected) = expected_source_snapshot {
                    if bound_source_snapshot(bound_source.as_deref()).await != Some(*expected) {
                        return Ok(OfflineProduceOutcome::SourceChanged);
                    }
                }
                let manifest = manifest.as_ref().expect("queue manifest");
                let completed = match fence
                    .complete(
                        self.store.as_ref(),
                        &hash,
                        &cached.relative_dir,
                        settled_bytes,
                        adopting_legacy.then_some(cached.bytes),
                        &manifest.manifest_digest,
                        unix_ms(),
                    )
                    .await
                {
                    Ok(completed) => completed,
                    Err(error) => {
                        tracing::warn!(target: "plurxd::transcode", recipe = %hash, %error, "queue cache-hit settlement unavailable");
                        return Ok(OfflineProduceOutcome::StoreUnavailable);
                    }
                };
                if !completed {
                    return Ok(OfflineProduceOutcome::Yielded("publication_refused"));
                }
            }
            if let Some(package_id) = offline_package_id {
                let claim_generation = offline_claim_generation
                    .ok_or("offline package production lost its claim generation")?;
                let current = match self
                    .store
                    .offline_package_claim_is_current(
                        package_id,
                        &cache.node_id,
                        claim_generation,
                        &hash,
                    )
                    .await
                {
                    Ok(current) => current,
                    Err(error) => {
                        tracing::warn!(target: "plurxd::transcode", package = package_id, %error, "offline cache-hit claim check unavailable");
                        return Ok(OfflineProduceOutcome::StoreUnavailable);
                    }
                };
                if !current {
                    return Ok(OfflineProduceOutcome::Yielded("ownership_lost"));
                }
            }
            return Ok(OfflineProduceOutcome::Cached(Produced {
                recipe: hash,
                bytes: settled_bytes,
                duration_ms: part.duration_ms(),
                segments: part.segments.len(),
                parts: 0,
            }));
        }
        drop(cache_lookup);

        if cancelled.is_some_and(tokio_util::sync::CancellationToken::is_cancelled) {
            return Ok(OfflineProduceOutcome::Yielded("ownership_lost"));
        }
        let subtitle_handle = self
            .ensure_text_subtitle(
                file,
                opts.subtitle_burn.as_ref(),
                crate::process_control::ChildWork::background(
                    "text subtitle for an offline package",
                ),
            )
            .await?;
        if cancelled.is_some_and(tokio_util::sync::CancellationToken::is_cancelled) {
            return Ok(OfflineProduceOutcome::Yielded("ownership_lost"));
        }
        let lease_generation = if let Some(fence) = publication_fence {
            let Some(lease) = fence.snapshot().await else {
                return Ok(OfflineProduceOutcome::Yielded("ownership_lost"));
            };
            Some(lease.fence)
        } else {
            None
        };
        let relative = if let Some(job) = &queue_job {
            format!("{}/{hash}-j{}-f{}", &hash[..2], job.id, job.fence)
        } else {
            lease_generation.map_or_else(
                || format!("{}/{hash}", &hash[..2]),
                |fence| format!("{}/{hash}-f{fence}", &hash[..2]),
            )
        };
        let temp = if let Some(job) = &queue_job {
            // Staging is node-local and job-stable so yielding and reclaiming
            // the same row on this node resumes its published part boundary.
            // The final generation remains fence-scoped below; a successor on
            // another node has a different local root and starts from zero.
            cache.dir.join("tmp").join(format!("{hash}-j{}", job.id))
        } else {
            lease_generation.map_or_else(
                || crate::cachekeep::staging_dir(&cache.dir, &hash),
                |fence| cache.dir.join("tmp").join(format!("{hash}-f{fence}")),
            )
        };
        let temp_parent = temp
            .parent()
            .ok_or("cache staging directory has no parent")?;
        let staging_identity = temp
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .ok_or("cache staging directory has no safe identity")?;
        // Take the shared-parent guard before checking or creating `tmp`.
        // Empty-parent cleanup is otherwise able to unlink it in the gap
        // between this check and the first staging child installation.
        let Some(_staging_guard) = self.cache_readers.begin_staging(staging_identity) else {
            return Ok(OfflineProduceOutcome::Yielded("staging_maintenance"));
        };
        ensure_cache_directory(&cache.dir, temp_parent).await?;
        let taken = if pretranscode_fence.is_some() {
            true
        } else if let Some(fence) = publication_fence {
            PublicationStore::fenced(self.store.as_ref(), fence.clone())
                .claim_cache_entry(
                    &hash,
                    file.id,
                    CACHE_RECIPE_VERSION,
                    &cache.node_id,
                    &relative,
                )
                .await
                .map_err(|error| error.to_string())?
        } else {
            self.store
                .claim_cache_entry(
                    &hash,
                    file.id,
                    CACHE_RECIPE_VERSION,
                    &cache.node_id,
                    &relative,
                )
                .await
                .map_err(|error| error.to_string())?
        };
        if cancelled.is_some_and(tokio_util::sync::CancellationToken::is_cancelled) {
            return Ok(OfflineProduceOutcome::Yielded("ownership_lost"));
        }
        if !taken {
            if plurx_core::fs_secure::SecureDirectory::open(&temp)
                .await
                .is_err()
            {
                tracing::debug!(
                    target: "plurxd::transcode",
                    recipe = %hash,
                    file = file.id,
                    "cache entry claimed elsewhere; standing down"
                );
                return Ok(OfflineProduceOutcome::ClaimedElsewhere);
            }
            tracing::info!(
                target: "plurxd::transcode",
                recipe = %hash,
                file = file.id,
                "resuming a portable transcode left unfinished"
            );
        }

        let staging_parent = plurx_core::fs_secure::SecureDirectory::open(temp_parent)
            .await
            .map_err(|error| format!("opening cache staging parent: {error}"))?;
        let mut staging = match staging_parent.open_child_directory(staging_identity).await {
            Ok(staging) => staging,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => staging_parent
                .create_child_directory(staging_identity)
                .await
                .map_err(|error| format!("creating cache staging root: {error}"))?,
            Err(error) => return Err(format!("opening cache staging root: {error}")),
        };
        if let Some(job) = &queue_job {
            let Some(source) = expected_source_snapshot.as_ref().copied() else {
                return Ok(OfflineProduceOutcome::SourceChanged);
            };
            staging = bind_pretranscode_staging(
                &staging_parent,
                staging_identity,
                staging,
                job,
                &hash,
                source,
            )
            .await?;
        }

        let mut published = match self
            .produce_into(&staging, &hash, &request, subtitle_handle.as_ref())
            .await
        {
            Ok(ProductionProgress::Ready(published)) => published,
            Ok(ProductionProgress::Yielded(reason)) => {
                if cancelled.is_some_and(tokio_util::sync::CancellationToken::is_cancelled) {
                    return Ok(OfflineProduceOutcome::Yielded("ownership_lost"));
                }
                if pretranscode_fence.is_some() {
                    // The queue heartbeat is the claim heartbeat. No cache
                    // location exists until fenced completion.
                } else if let Some(fence) = publication_fence {
                    if let Err(error) = PublicationStore::fenced(self.store.as_ref(), fence.clone())
                        .touch_cache_claim(&hash, &cache.node_id)
                        .await
                    {
                        tracing::warn!(target: "plurxd::transcode", recipe = %hash, error = %error, "could not mark a fenced pre-transcode as still in progress");
                    }
                } else {
                    self.touch_claim(&hash, &cache.node_id).await;
                }
                return Ok(OfflineProduceOutcome::Yielded(reason));
            }
            Err(error) => {
                // A failed assembly may have created another generation-sized
                // set of hardlinks. Remove those capability-relative children
                // first so the outer staging quarantine retains a fixed shape
                // of parts + playlists + identity within its 120,100-entry
                // ceiling.
                let _ = remove_staged_child(&staging, ASSEMBLED_TEMP_DIR).await;
                let _ = remove_staged_child(&staging, ASSEMBLED_DIR).await;
                let _ = quarantine_remove_cache_tree(&temp, 3).await;
                if !cancelled.is_some_and(tokio_util::sync::CancellationToken::is_cancelled)
                    && pretranscode_fence.is_none()
                {
                    if let Some(fence) = publication_fence {
                        // Cancelled: the produce path is already returning its
                        // source error; this cleanup failure is not that cause.
                        crate::store_result::observe(
                            crate::store_result::Operation::ForgetFailedFencedOfflineCacheEntry,
                            crate::store_result::Discard::Cancelled,
                            PublicationStore::fenced(self.store.as_ref(), fence.clone())
                                .forget_cache_entry(&hash, &cache.node_id, "local")
                                .await,
                        );
                    } else {
                        // Cancelled: the produce path is already returning its
                        // source error; this cleanup failure is not that cause.
                        crate::store_result::observe(
                            crate::store_result::Operation::ForgetFailedOfflineCacheEntry,
                            crate::store_result::Discard::Cancelled,
                            self.store
                                .forget_cache_entry(&hash, &cache.node_id, "local")
                                .await,
                        );
                    }
                }
                return Err(error);
            }
        };

        if cancelled.is_some_and(tokio_util::sync::CancellationToken::is_cancelled) {
            return Ok(OfflineProduceOutcome::Yielded("ownership_lost"));
        }
        if let Some(expected) = expected_policy_generation.as_deref() {
            if let Some(outcome) = self.pretranscode_policy_interruption(expected).await {
                return Ok(outcome);
            }
        }
        if let Some(expected) = expected_source_snapshot {
            if bound_source_snapshot(bound_source.as_deref()).await != Some(*expected) {
                return Ok(OfflineProduceOutcome::SourceChanged);
            }
        }
        let generation = staging
            .open_child_directory(ASSEMBLED_DIR)
            .await
            .map_err(|error| format!("opening assembled generation: {error}"))?;
        // A queue job publishes a manifest because the queue's readers require
        // one. Under the qualified identity every publication needs one, for a
        // different reason: the retention rule reads the manifest, so a
        // generation without one cannot be kept at all — speculative warming
        // and offline preparation would settle a clean receipt and then have
        // it refused for having written it nowhere.
        //
        // Deliberately not extended to the unqualified identity. A row that
        // records a manifest digest is offered to cluster placement, is
        // enrolled in the integrity scrub, and serves offline segments fatally
        // rather than leniently on pre-existing bit rot — three behaviour
        // changes for rows deployed nodes already hold, in exchange for a
        // receipt nothing there consults. (Shared-cache fanout would have been
        // a fourth; it is refused outright below, under either identity.)
        // Under the qualified identity none of those rows exist yet, so each
        // of those becomes a property of a new key space rather than a change
        // to a live one.
        let manifest = if queue_job.is_some() || plan.enforces_receipt() {
            let generation_id = match &queue_job {
                Some(job) => format!("{}:{}", job.id, job.fence),
                // Off the queue there is no job to name it after, so the
                // final directory's own identity names it. What matters is
                // that it is *stable across the resume passes of one
                // production*: the digest checkpoint a preempted manifest hash
                // leaves behind is keyed by this id, and a value that changed
                // per pass would silently rehash the whole film every time.
                //
                // Unfenced it is the recipe hash, which is also stable across
                // *unrelated* productions of the same recipe — and that is
                // safe for three reasons worth writing down, because the whole
                // argument rests on them. The checkpoint file lives inside the
                // generation directory, so it dies with the staging tree it
                // describes. Its header must carry this same id or the
                // checkpoint is discarded. And every object digest in it is
                // reused only while that file's device, inode, size and mtime
                // still match, so re-encoded bytes are rehashed rather than
                // certified from a record of bytes that no longer exist.
                None => identity_for(&relative)?.to_owned(),
            };
            let names = std::iter::once("index.m3u8".to_owned())
                .chain((0..published.segments).map(|index| format!("seg{index:05}.ts")))
                .collect::<Vec<_>>();
            // Which lanes owe the idle courtesy. Both background lanes do —
            // the queue and speculative warming exist to be interrupted by
            // people pressing play. An offline package does not: a user is
            // waiting on that download, and `pretranscode_worker_idle` is
            // false while one is waiting, so applying it there would yield on
            // the first check and every check after it, and the package would
            // never become ready.
            let owes_idle_courtesy = offline_package_id.is_none();
            let mut manifest_yield_reason = None;
            let manifest = plurx_core::transcode::manifest::publish_controlled_directory(
                &generation,
                &generation_id,
                &names,
                // The join of every contributing part's receipt, and `None`
                // when this pass adopted an assembly it did not make. This is
                // the only place a producer receipt reaches durable storage.
                published.health.clone(),
                || {
                    manifest_yield_reason = if cancelled
                        .is_some_and(tokio_util::sync::CancellationToken::is_cancelled)
                    {
                        Some("ownership_lost")
                    } else if Instant::now() >= request.deadline {
                        Some("production_deadline")
                    } else if owes_idle_courtesy {
                        self.pretranscode_publication_yield_reason()
                    } else {
                        None
                    };
                    manifest_yield_reason.is_some()
                },
            )
            .await?;
            let Some(manifest) = manifest else {
                return Ok(OfflineProduceOutcome::Yielded(
                    manifest_yield_reason.unwrap_or("manifest_interrupted"),
                ));
            };
            self.hooks.get().manifest_published();
            if let Some(expected) = expected_policy_generation.as_deref() {
                if let Some(outcome) = self.pretranscode_policy_interruption(expected).await {
                    return Ok(outcome);
                }
            }
            // The manifest file is bytes on disk like any other. Charging it
            // to the ledger on one path and not the other would leave the cache
            // budget under-counting every generation the other path made, with
            // no reconciliation anywhere.
            published.bytes = published.bytes.saturating_add(
                generation
                    .child_metadata(plurx_core::transcode::manifest::MANIFEST_FILE)
                    .await
                    .map(|metadata| metadata.identity.size.min(i64::MAX as u64) as i64)
                    .unwrap_or(0),
            );
            Some(manifest)
        } else {
            None
        };
        let final_dir = cache.dir.join(&relative);
        // Protect both recipe eviction and the final path across rename ->
        // durable completion. This is intentionally acquired before ensuring
        // the shared fanout parent: the parent guard closes its otherwise
        // empty ensure -> child-install race with orphan cleanup.
        let identity = identity_for(&relative)?;
        let Some(publication_guard) = self.cache_readers.begin_publication(&hash, identity) else {
            return Ok(OfflineProduceOutcome::Yielded("publication_maintenance"));
        };
        let final_parent = final_dir.parent().ok_or("final generation has no parent")?;
        ensure_cache_directory(&cache.dir, final_parent).await?;
        if cancelled.is_some_and(tokio_util::sync::CancellationToken::is_cancelled) {
            return Ok(OfflineProduceOutcome::Yielded("ownership_lost"));
        }
        if let Some(expected) = expected_policy_generation.as_deref() {
            if let Some(outcome) = self.pretranscode_policy_interruption(expected).await {
                return Ok(outcome);
            }
        }
        if let Some(expected) = expected_source_snapshot {
            if bound_source_snapshot(bound_source.as_deref()).await != Some(*expected) {
                return Ok(OfflineProduceOutcome::SourceChanged);
            }
        }
        staging
            .rename_child_to(ASSEMBLED_DIR, final_parent, identity)
            .await
            .map_err(|error| format!("publishing {}: {error}", final_dir.display()))?;
        // The same rule, applied before any write that would let a future
        // request find these bytes. The session that asked for them already
        // has them: the generation is assembled and renamed into place above,
        // and refusing here declines to *keep* it, never to serve it.
        //
        // It reads the manifest, never an in-memory value, because the
        // manifest is what the next request will have to re-read: a receipt
        // this process never wrote down cannot certify these bytes to anyone
        // tomorrow. Every path that publishes under this identity now writes
        // one, so a refusal here is a refusal about what the receipt *says*
        // rather than about where it was filed.
        if !generation_permits_reuse(plan, manifest.as_ref()) {
            tracing::warn!(
                target: "plurxd::transcode",
                recipe = %hash,
                namespace = plan.artifact_namespace(),
                manifest = manifest.is_some(),
                qualification = manifest
                    .as_ref()
                    .and_then(|manifest| manifest.producer_health.as_ref())
                    .map_or("absent", |receipt| receipt.qualification.name()),
                terminal_fault = manifest
                    .as_ref()
                    .and_then(|manifest| manifest.producer_health.as_ref())
                    .and_then(|receipt| receipt.terminal_fault)
                    .map(crate::decoder_health::DecodeFaultKind::name),
                "refusing to retain a generation whose producer health receipt does not permit reuse"
            );
            let _ = quarantine_remove_cache_tree(&final_dir, 1).await;
            drop(publication_guard);
            let _ = quarantine_remove_cache_tree(&temp, 3).await;
            // The same cleanup the failure arm does, and for a sharper reason.
            // A refused generation leaves the `complete = 0` claim behind
            // unless this runs, and that claim is not merely litter: the next
            // request for this recipe cannot claim it, finds no staging tree
            // to resume, and stands down as `ClaimedElsewhere` — while
            // `stale_cache_claims` refuses to reap a claim whose package is
            // still queued. The package holds the claim and the claim holds
            // the package, forever.
            if pretranscode_fence.is_none() {
                forget_unfenced_claim_with(
                    self.store.as_ref(),
                    &hash,
                    &cache.node_id,
                    publication_fence.as_ref(),
                )
                .await;
            }
            return Ok(OfflineProduceOutcome::HealthRefused);
        }
        if let (Some(fence), Some(manifest)) = (pretranscode_fence.as_ref(), manifest.as_ref()) {
            if let Some(expected) = expected_policy_generation.as_deref() {
                if let Some(outcome) = self.pretranscode_policy_interruption(expected).await {
                    let _ = quarantine_remove_cache_tree(&final_dir, 1).await;
                    return Ok(outcome);
                }
            }
            if let Some(expected) = expected_source_snapshot {
                if bound_source_snapshot(bound_source.as_deref()).await != Some(*expected) {
                    let _ = quarantine_remove_cache_tree(&final_dir, 1).await;
                    return Ok(OfflineProduceOutcome::SourceChanged);
                }
            }
            let completed = match fence
                .complete(
                    self.store.as_ref(),
                    &hash,
                    &relative,
                    published.bytes,
                    None,
                    &manifest.manifest_digest,
                    unix_ms(),
                )
                .await
            {
                Ok(completed) => completed,
                Err(error) => {
                    tracing::warn!(target: "plurxd::transcode", recipe = %hash, %error, "queue completion settlement unavailable");
                    return Ok(OfflineProduceOutcome::StoreUnavailable);
                }
            };
            if !completed {
                let _ = quarantine_remove_cache_tree(&final_dir, 1).await;
                return Ok(OfflineProduceOutcome::Yielded("publication_refused"));
            }
        } else {
            // Every queue completion is settled by the branch above, because
            // `queue_job` is `Some` exactly when `pretranscode_fence` is.
            debug_assert!(
                queue_job.is_none(),
                "a queue completion must settle through its own fence"
            );
            // A digest only where the identity asks for one. Deriving it from
            // the manifest alone would work today and would put the safety in
            // the invariant asserted above rather than here: a queue job that
            // ever reached this branch would newly stamp a digest onto an
            // unqualified row, and a row that has one is scrubbed, is offered
            // to placement, and serves offline segments fatally rather than
            // leniently. Those must not move for rows deployed nodes already
            // hold.
            let digest = plan
                .enforces_receipt()
                .then(|| {
                    manifest
                        .as_ref()
                        .map(|manifest| manifest.manifest_digest.as_str())
                })
                .flatten();
            if let Some(package_id) = offline_package_id {
                let claim_generation = offline_claim_generation
                    .ok_or("offline package production lost its claim generation")?;
                let completed = match self
                    .store
                    .complete_offline_cache_entry(
                        package_id,
                        &cache.node_id,
                        claim_generation,
                        &hash,
                        published.bytes,
                        digest,
                    )
                    .await
                {
                    Ok(completed) => completed,
                    Err(error) => {
                        tracing::warn!(target: "plurxd::transcode", package = package_id, recipe = %hash, %error, "offline cache completion unavailable");
                        return Ok(OfflineProduceOutcome::StoreUnavailable);
                    }
                };
                if !completed {
                    let _ = quarantine_remove_cache_tree(&final_dir, 1).await;
                    return Ok(OfflineProduceOutcome::Yielded("publication_refused"));
                }
            } else if let Some(fence) = publication_fence {
                PublicationStore::fenced(self.store.as_ref(), fence.clone())
                    .complete_cache_entry(&hash, &cache.node_id, &relative, published.bytes, digest)
                    .await
                    .map_err(|error| error.to_string())?;
            } else {
                self.store
                    .complete_cache_entry(&hash, &cache.node_id, published.bytes, digest)
                    .await
                    .map_err(|error| error.to_string())?;
            }
        }
        // Queue-owned generations only, exactly as before. A manifest is now
        // published off the queue too, and that alone must not enrol a
        // speculative or offline generation in shared-cluster fanout: that is
        // a second full copy onto a shared mount, other nodes routing work to
        // it, and cluster quota — a decision of its own, not a side effect of
        // carrying a receipt.
        if let (Some(shared_cache), Some(manifest), true) = (
            self.shared_cache.as_ref(),
            manifest.as_ref(),
            queue_job.is_some(),
        ) {
            match shared_cache
                .publish_generation(&hash, file.id, CACHE_RECIPE_VERSION, &final_dir, manifest)
                .await
            {
                Ok(true) => tracing::info!(
                    target: "plurxd::transcode",
                    recipe = %hash,
                    generation = %manifest.generation_id,
                    "portable transcode published to the shared cache"
                ),
                Ok(false) => {}
                Err(error) => tracing::warn!(
                    target: "plurxd::transcode",
                    recipe = %hash,
                    generation = %manifest.generation_id,
                    %error,
                    "shared cache publication failed; the node-local generation remains ready"
                ),
            }
        }
        drop(publication_guard);
        let _ = quarantine_remove_cache_tree(&temp, 3).await;
        tracing::info!(
            target: "plurxd::transcode",
            recipe = %hash,
            file = file.id,
            height = opts.target_height,
            bytes = published.bytes,
            duration_s = published.duration_ms / 1000,
            segments = published.segments,
            parts = published.parts,
            "portable transcode published"
        );
        Ok(OfflineProduceOutcome::Ready(Produced {
            recipe: hash,
            bytes: published.bytes,
            duration_ms: published.duration_ms,
            segments: published.segments,
            parts: published.parts,
        }))
    }

    /// Keep a resumable claim from ageing into a crash leftover.
    ///
    /// Without this a film that needs several passes would have its bookmark
    /// swept a day after the first one, and every pass after that would start
    /// from zero — the loop that never finishes, wearing the disguise of a
    /// cleanup working correctly.
    async fn touch_claim(&self, hash: &str, node_id: &str) {
        if let Err(e) = self.store.touch_cache_claim(hash, node_id).await {
            tracing::warn!(target: "plurxd::transcode", recipe = %hash, error = %e, "could not mark a pre-transcode as still in progress");
        }
    }

    /// Encode into `temp` until finished, out of time, or out of patience with
    /// being preempted. A yield retains the cause observed when work stopped.
    pub(super) async fn produce_into(
        &self,
        temp: &plurx_core::fs_secure::SecureDirectory,
        hash: &str,
        request: &PortableProduction<'_>,
        subtitle_handle: Option<&std::fs::File>,
    ) -> Result<ProductionProgress, String> {
        let PortableProduction {
            file,
            opts,
            plan,
            deadline,
            yield_to_offline,
            cancelled,
            offline_package_id,
            offline_claim_generation,
            publication_fence: _,
            pretranscode_fence,
            expected_policy_generation: _,
            expected_source_snapshot: _,
            bound_source,
        } = request.clone();
        let encoder = plan.encoder();
        let max = self.max_hw_sessions().await;
        // Whatever an earlier pass got through. Usually nothing; on a busy box
        // making a long film, this is how it eventually finishes.
        let plan_digest = plan.plan_digest();
        let ResumedParts {
            mut parts,
            receipts: inherited_receipts,
        } = resume_parts(temp, &plan_digest).await?;
        let mut generation_health = GenerationObservation::inheriting(
            inherited_receipts,
            carried_generation_health(temp, &plan_digest).await,
            &plan_digest,
        );
        let mut retained_segments = parts.iter().map(|part| part.segments.len()).sum::<usize>();
        let mut retained_duration_ms = parts
            .iter()
            .try_fold(0_i64, |total, part| total.checked_add(part.duration_ms()))
            .ok_or("retained transcode duration overflow")?;
        if let Ok(assembled) = temp.open_child_directory(ASSEMBLED_DIR).await {
            if let Some(published) =
                assembled_publication(&assembled, &parts, generation_health.settle()).await
            {
                tracing::info!(
                    target: "plurxd::transcode",
                    recipe = %hash,
                    segments = published.segments,
                    "resuming an assembled generation awaiting integrity publication"
                );
                return Ok(ProductionProgress::Ready(published));
            }
        }
        if !parts.is_empty() {
            tracing::info!(
                target: "plurxd::transcode",
                recipe = %hash, parts = parts.len(),
                from_s = crate::produce::resume_at_ms(&parts) / 1000,
                "picking up where an earlier pass stopped"
            );
        }
        // Counted separately from `parts` because a part that was killed
        // before its first segment produced nothing and so is not one — but it
        // did start an encoder, which is the thing worth bounding.
        let mut spawned = 0usize;

        while spawned < PRODUCER_MAX_PARTS {
            if parts.len() >= MAX_RETAINED_PART_DIRECTORIES {
                return Err("retained transcode exceeds its part bound".to_owned());
            }
            if cancelled.is_some_and(tokio_util::sync::CancellationToken::is_cancelled) {
                return Ok(ProductionProgress::Yielded("ownership_lost"));
            }
            if yield_to_offline
                && self
                    .offline_waiting
                    .load(std::sync::atomic::Ordering::Acquire)
            {
                return Ok(ProductionProgress::Yielded("offline_waiting"));
            }
            if Instant::now() >= deadline {
                tracing::debug!(
                    target: "plurxd::transcode",
                    recipe = %hash, "producer out of time for this run"
                );
                return Ok(ProductionProgress::Yielded("production_deadline"));
            }
            // Do not even start while a viewer is queuing — and never spend
            // what a viewer would want.
            //
            // For a hardware encoder the slot request answers both, since
            // background acquisition is refused outright while anyone is in
            // the queue. Software parts draw from the CPU pool at Background
            // priority now (§2.4): the same refusal while a viewer queues —
            // which also keeps this loop from spawning ffmpeg just to kill it
            // on the first poll, hundreds of times a second — plus a real
            // reservation, so producer parts and live software sessions can
            // no longer oversubscribe every core between them.
            let mut sw_hold = None;
            let slot = if pretranscode_fence.is_some() {
                // The shared-queue claim already holds both real permits.
                None
            } else if encoder == Encoder::Software {
                match self.admissions.try_admit_software(
                    self.software_budget().await,
                    Workload::of(file, opts.target_height).software_threads(),
                    Priority::Background,
                ) {
                    Some(permit) => {
                        sw_hold = Some(permit);
                        None
                    }
                    None => {
                        tokio::time::sleep(self.producer.retry).await;
                        continue;
                    }
                }
            } else {
                match self.admissions.try_acquire(max, Priority::Background) {
                    Some(slot) => Some(slot),
                    None => {
                        tokio::time::sleep(self.producer.retry).await;
                        continue;
                    }
                }
            };
            spawned += 1;

            let part_name = crate::produce::part_dir(parts.len());
            let part_dir = temp
                .create_child_directory(&part_name)
                .await
                .map_err(|e| format!("creating {part_name}: {e}"))?;
            let resume_ms = crate::produce::resume_at_ms(&parts);
            let part_opts = TranscodeOptions {
                start_seconds: resume_ms as f64 / 1000.0,
                // What the Background permit reserved is what this part may
                // spend. Not part of the recipe hash, so resumed parts and
                // cache identity are unaffected.
                software_threads: pretranscode_fence
                    .as_ref()
                    .map(|fence| fence.admission.threads as u32)
                    .or_else(|| sw_hold.as_ref().map(|p| p.threads() as u32)),
                ..opts.clone()
            };
            let source_offset_permit = if let Some(source) = &bound_source {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Ok(ProductionProgress::Yielded("production_deadline"));
                }
                Some(tokio::select! {
                    biased;
                    _ = async {
                        match cancelled {
                            Some(cancelled) => cancelled.cancelled().await,
                            None => std::future::pending::<()>().await,
                        }
                    } => return Ok(ProductionProgress::Yielded("ownership_lost")),
                    permit = tokio::time::timeout(
                        remaining,
                        Arc::clone(&source.offset_gate).acquire_owned(),
                    ) => {
                        permit
                            .map_err(|_| "timed out waiting for bound-source offset ownership")?
                            .map_err(|_| "bound-source offset owner closed")?
                    }
                })
            } else {
                None
            };
            // Unpaced, deliberately. Pacing exists so a live session does not
            // write a film ahead of a playhead that will never reach it; a
            // producer has no playhead and every second it spends holding the
            // hardware is a second a viewer might want it. The value is
            // [`ProducerTuning::pacing`], which is `unpaced()` everywhere
            // except the one test that has to interrupt this encoder.
            #[cfg(unix)]
            let mut descriptor_file;
            #[cfg(unix)]
            let (ffmpeg_file, bound_source_fd) = if let Some(source) = &bound_source {
                use std::os::fd::AsRawFd;
                descriptor_file = file.clone();
                descriptor_file.path = std::path::PathBuf::from("/dev/fd/3");
                (&descriptor_file, Some(source.handle.as_raw_fd()))
            } else {
                (file, None)
            };
            #[cfg(windows)]
            let mut descriptor_file;
            #[cfg(windows)]
            let ffmpeg_file = if let Some(source) = &bound_source {
                if bound_source_snapshot(Some(source)).await != Some(source.snapshot) {
                    return Ok(ProductionProgress::Yielded("source_changed"));
                }
                descriptor_file = file.clone();
                descriptor_file.path = plurx_core::fs_secure::std_file_path(&source.handle)
                    .map_err(|error| format!("resolving held Windows source path: {error}"))?;
                &descriptor_file
            } else {
                file
            };
            // Linux resolves descendants below a directory descriptor through
            // procfs. Darwin's fdesc filesystem reopens `/dev/fd/4` itself but
            // does not resolve `/dev/fd/4/child`; `spawn_ffmpeg` therefore
            // anchors the macOS child cwd to descriptor 4 and the muxer uses a
            // relative path. Both routes remain bound to the held directory.
            #[cfg(unix)]
            let output_directory = if cfg!(target_os = "macos") {
                part_name.clone()
            } else {
                format!("/dev/fd/4/{part_name}")
            };
            #[cfg(windows)]
            let output_directory = temp.path().join(&part_name).to_string_lossy().into_owned();
            let execution = TranscodeExecution::from_options(
                ffmpeg_file,
                &part_opts,
                self.producer.pacing,
                &output_directory,
            )
            .map_err(|error| error.to_string())?;
            #[cfg(windows)]
            let mut execution = execution;
            #[cfg(windows)]
            if let Some(subtitle) = subtitle_handle {
                execution.subtitle_file = Some(
                    plurx_core::fs_secure::std_file_path(subtitle).map_err(|error| {
                        format!("resolving held Windows subtitle path: {error}")
                    })?,
                );
            }
            let observation = DiagnosticObservation::for_plan(
                plan,
                &self.measured_decoders,
                self.automatic_decoder_recovery_enabled(),
            );
            let execution = execution.observing_qualified_grammar(observation.qualified_logging());
            let args = transcode::hls_args(plan, &execution);
            tracing::info!(
                target: "plurxd::transcode",
                recipe = %hash, part = parts.len(), from_s = part_opts.start_seconds,
                encoder = encoder.label(), "pre-transcode part starting"
            );
            let progress = Arc::new(Progress::new());
            let generation = progress.begin_attempt();
            let (mut child, _child_job, diagnostics) = spawn_ffmpeg(
                &args,
                crate::process_control::ChildWork::background("pre-transcode cache producer"),
                encoder.label(),
                hash,
                FfmpegProgressObserver::offline(Arc::clone(&progress), generation),
                &self.runtime_cache,
                {
                    #[cfg(unix)]
                    let descriptors = FfmpegDescriptors::from_raw_fds(
                        bound_source_fd,
                        Some(temp.raw_fd()),
                        subtitle_handle.map(std::os::fd::AsRawFd::as_raw_fd),
                        true,
                    );
                    #[cfg(windows)]
                    let descriptors = windows_offline_descriptors(
                        bound_source.as_deref(),
                        &part_dir,
                        subtitle_handle,
                    )?;
                    descriptors
                },
                observation.clone(),
            )?
            .into_parts();

            let ended = self
                .run_part(&mut child, deadline, yield_to_offline, cancelled)
                .await;
            drop(source_offset_permit);
            drop(slot); // before anything else: a viewer is probably waiting on it
            drop(sw_hold); // and the pool share with it
                           // The part's own observation, settled after the permits are back:
                           // the child is already reaped, so the drain is normally instant,
                           // and holding a hardware slot through a bounded wait for a reader
                           // is exactly the trade the comment above refuses. M3b records the
                           // receipt; M3c is what makes it refuse a part.
            let receipt = diagnostics
                .settle(
                    crate::decoder_health::DIAGNOSTIC_DRAIN_BUDGET,
                    part_exit_disposition(&ended),
                )
                .await;
            report_producer_health(&format!("{hash} part {}", parts.len()), &receipt);
            let ValidatedPart { part, shape, .. } = read_part(&part_dir).await;
            let produced = !part.is_empty();
            if produced {
                // Sealed beside the bytes it describes, so whichever pass
                // resumes this film does not have to call the part unobserved.
                retain_part_health(&part_dir, &shape, &receipt).await;
            }
            // Recorded whether or not it produced. An attempt that decoded
            // nothing and exited zero writes no segment, and that receipt is
            // the one that matters most.
            generation_health.record(receipt, produced);
            if !produced {
                // Its directory is about to be removed or reused by the retry,
                // so there is nothing for it to be sealed beside. The ledger is
                // written now rather than at the end of the pass, because a
                // pass that is about to be preempted is exactly the one whose
                // observation would otherwise be lost.
                if let Some(unproductive) = generation_health.unproductive() {
                    retain_generation_health(temp, unproductive).await;
                }
            }
            if produced {
                if parts.len().saturating_add(1) > MAX_RETAINED_PART_DIRECTORIES {
                    let _ = remove_staged_child(temp, &part_name).await;
                    return Err("produced transcode exceeds its part bound".to_owned());
                }
                let next_segments = retained_segments.saturating_add(part.segments.len());
                let next_duration_ms = retained_duration_ms
                    .checked_add(part.duration_ms())
                    .ok_or("produced transcode duration overflow")?;
                if next_segments >= plurx_core::transcode::manifest::MAX_OBJECTS
                    || next_duration_ms > MAX_RETAINED_TOTAL_DURATION_MS
                {
                    let _ = remove_staged_child(temp, &part_name).await;
                    return Err(
                        "produced transcode exceeds its aggregate generation bound".to_owned()
                    );
                }
                retained_segments = next_segments;
                retained_duration_ms = next_duration_ms;
                parts.push(part);
                if let (Some(package_id), Some(duration_ms)) = (
                    offline_package_id,
                    file.duration_ms.filter(|duration| *duration > 0),
                ) {
                    let claim_generation = offline_claim_generation
                        .expect("offline package progress requires its claim generation");
                    let completed_ms = crate::produce::resume_at_ms(&parts);
                    let progress = completed_ms
                        .saturating_mul(1000)
                        .saturating_div(duration_ms)
                        .clamp(1, 999);
                    // Best effort: a missed intermediate percentage cannot
                    // invalidate the resumable parts or final settlement.
                    crate::store_result::observe(
                        crate::store_result::Operation::UpdateOfflineProgressTranscoding,
                        crate::store_result::Discard::BestEffort,
                        self.store
                            .update_offline_progress(
                                package_id,
                                self.offline_owner(),
                                claim_generation,
                                "transcoding",
                                progress,
                            )
                            .await,
                    );
                }
            }

            match ended {
                PartEnd::Finished => {
                    if cancelled.is_some_and(tokio_util::sync::CancellationToken::is_cancelled) {
                        return Ok(ProductionProgress::Yielded("ownership_lost"));
                    }
                    return publish_from(temp, &parts, generation_health.settle())
                        .await
                        .map(|published| {
                            published.map_or(
                                ProductionProgress::Yielded("assembly_incomplete"),
                                ProductionProgress::Ready,
                            )
                        });
                }
                PartEnd::Preempted(_) | PartEnd::Deadline => {
                    let reason = match ended {
                        PartEnd::Preempted(reason) => reason,
                        PartEnd::Deadline => "production_deadline",
                        _ => unreachable!(),
                    };
                    tracing::info!(
                        target: "plurxd::transcode",
                        recipe = %hash, spawned,
                        produced_s = crate::produce::resume_at_ms(&parts) / 1000,
                        "pre-transcode yielded"
                    );
                    // A part that produced nothing leaves an empty directory
                    // that the next part must not reuse a number with.
                    if !produced {
                        let _ = remove_staged_child(temp, &part_name).await;
                    }
                    if pretranscode_fence.is_some() {
                        return Ok(ProductionProgress::Yielded(reason));
                    }
                    if yield_to_offline
                        && self
                            .offline_waiting
                            .load(std::sync::atomic::Ordering::Acquire)
                    {
                        return Ok(ProductionProgress::Yielded("offline_waiting"));
                    }
                    if cancelled.is_some_and(tokio_util::sync::CancellationToken::is_cancelled) {
                        return Ok(ProductionProgress::Yielded("ownership_lost"));
                    }
                    if matches!(ended, PartEnd::Deadline) {
                        // Out of budget for this pass. Nothing is published —
                        // a partial asset must never be serveable — but
                        // nothing is discarded either: the numbered parts
                        // stay on disk under the claim, and the next pass
                        // over this recipe starts at `resume_parts`, picking
                        // up exactly where this one stopped (the log line at
                        // the top of this loop is that event). A film that
                        // cannot finish inside one window therefore still
                        // gets cached — across as many passes as it takes —
                        // provided the claim row survives between them; the
                        // parts and the claim are the checkpoint, and there
                        // is deliberately no second bookmark in the database
                        // to disagree with them after a crash.
                        return Ok(ProductionProgress::Yielded("production_deadline"));
                    }
                }
                PartEnd::Failed(why) => return Err(why),
            }
        }
        tracing::warn!(
            target: "plurxd::transcode",
            recipe = %hash, parts = parts.len(),
            "pre-transcode preempted too many times; giving up on this run"
        );
        Ok(ProductionProgress::Yielded("preemption_limit"))
    }

    /// Run one part to completion, or until a viewer wants the hardware.
    pub(super) async fn run_part(
        &self,
        child: &mut Child,
        deadline: Instant,
        yield_to_offline: bool,
        cancelled: Option<&tokio_util::sync::CancellationToken>,
    ) -> PartEnd {
        loop {
            if cancelled.is_some_and(tokio_util::sync::CancellationToken::is_cancelled) {
                let _ = child.kill().await;
                return PartEnd::Preempted("ownership_lost");
            }
            match child.try_wait() {
                Ok(Some(status)) if status.success() => return PartEnd::Finished,
                Ok(Some(status)) => {
                    return PartEnd::Failed(format!("producer ffmpeg exited with {status}"))
                }
                Ok(None) => {}
                Err(e) => return PartEnd::Failed(format!("waiting on producer ffmpeg: {e}")),
            }
            // Checkpoint and terminate. Not SIGSTOP: a stopped ffmpeg still
            // holds the hardware codec session, so the viewer this is yielding
            // to would be blocked by a process that is doing nothing.
            if self.admissions.live_is_waiting() {
                let _ = child.kill().await;
                return PartEnd::Preempted("foreground_demand");
            }
            if yield_to_offline
                && self
                    .offline_waiting
                    .load(std::sync::atomic::Ordering::Acquire)
            {
                let _ = child.kill().await;
                return PartEnd::Preempted("offline_waiting");
            }
            if Instant::now() >= deadline {
                let _ = child.kill().await;
                return PartEnd::Deadline;
            }
            tokio::time::sleep(PRODUCER_POLL).await;
        }
    }
}
