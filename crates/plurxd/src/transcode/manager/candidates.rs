use super::*;
use plurx_core::playback::candidate::{CandidateId, CandidateRoute, QualityCandidate};

pub(crate) struct ResolvedQualityCatalog {
    pub(crate) candidates: Vec<QualityCandidate>,
    pub(crate) measured_candidate_outputs: Vec<crate::vodserve::retained::MeasuredCandidateOutput>,
    /// Actual selected construction availability, never a public permission.
    pub(crate) selected_unavailable: Option<String>,
}

#[derive(Debug)]
pub(crate) enum CandidateRestoreError {
    Incompatible(String),
    Unavailable(String),
}

impl CandidateRestoreError {
    pub(crate) fn is_incompatible(&self) -> bool {
        matches!(self, Self::Incompatible(_))
    }
}

impl std::fmt::Display for CandidateRestoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Incompatible(message) | Self::Unavailable(message) => {
                formatter.write_str(message)
            }
        }
    }
}

pub(super) fn copy_candidate_grade(file: &plurx_core::domain::MediaFile) -> OutputGrade {
    match transcode::routing_hdr(file) {
        Some("hlg" | "hdr10" | "hdr10plus" | "dolby_vision") => OutputGrade::Hdr10,
        _ => OutputGrade::Sdr,
    }
}

/// Catalog and dispatch share the exact pre-existing canonical copy identity.
/// This is a recipe equality check, not measured-output authority.
#[allow(clippy::too_many_arguments)]
pub(super) fn copy_candidate_recipe_digest(
    file: &plurx_core::domain::MediaFile,
    audio: Option<i64>,
    audio_offset_ms: i64,
    subtitle: Option<i64>,
    copy: (bool, bool, bool),
    source_version: Option<&str>,
    executable: Option<&str>,
    runtime_engine: Option<&str>,
    raster: (u32, u32),
) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"plurx:auto-quality-copy-fmp4:v1\0");
    hash.update(
        serde_json::to_vec(&serde_json::json!([
            file.id,
            file.size,
            file.mtime,
            &file.video_codec,
            &file.video_profile,
            audio,
            audio_offset_ms,
            subtitle,
            copy.0,
            copy.1,
            copy.2,
            file.dolby_vision,
            source_version,
            executable,
            runtime_engine,
            raster.0,
            raster.1,
            transcode::routing_hdr(file),
        ]))
        .expect("bounded candidate source identity is serializable"),
    );
    hash.finalize().into()
}

impl TranscodeManager {
    pub(crate) async fn measured_candidate_cost(
        &self,
        candidate: &QualityCandidate,
        request: &SessionRequest,
        held_source: Option<&crate::fragment_index_cluster::SourceFence>,
    ) -> Option<crate::vodserve::retained::MeasuredCandidateCostProof> {
        let mut resolved = request.clone();
        let file = if held_source.is_none() || resolved.audio_delivery.is_none() {
            Some(self.store.get_file(request.file_id).await.ok()??)
        } else {
            None
        };
        if resolved.audio_delivery.is_none() {
            let file = file.as_ref()?;
            let claim = request.audio_claim.as_ref()?;
            let selected = request.audio_index.map_or_else(
                || file.audio_streams.first(),
                |index| {
                    file.audio_streams
                        .iter()
                        .find(|stream| stream.index == index)
                },
            );
            let route = match candidate.route {
                CandidateRoute::Remux => plurx_core::playback::audio::AudioRoute::Progressive,
                CandidateRoute::Encode => plurx_core::playback::audio::AudioRoute::EncodedVod,
                CandidateRoute::Original => return None,
            };
            resolved.audio_delivery = Some(plurx_core::playback::audio::resolve_audio(
                selected,
                &claim.profile(),
                route,
                request.audio_offset_ms,
            ));
        }
        let opened;
        let source = if let Some(source) = held_source {
            source
        } else {
            opened = crate::fragment_index_cluster::open_source_fence(file.as_ref()?, None)
                .await
                .ok()?;
            &opened
        };
        self.vod
            .measured_candidate_cost(candidate, &resolved, source)
    }
    /// Companion PUBLIC descriptor projection from the same actual resolution.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn quality_candidates_with_measured_outputs(
        &self,
        file: &plurx_core::domain::MediaFile,
        caps: &plurx_core::playback::DeviceCaps,
        audio: Option<i64>,
        audio_offset_ms: i64,
        subtitle: Option<i64>,
        presentation: Presentation,
        retained_copy: Option<(bool, bool, bool)>,
        retained_audio: Option<&plurx_core::playback::audio::AudioDelivery>,
        retained_claim: Option<&plurx_core::playback::audio::AudioClaim>,
    ) -> ResolvedQualityCatalog {
        let Ok(Some(snapshot)) = self
            .store
            .playback_planning_snapshot(file.id, &QUALITY_PLANNING_KEYS)
            .await
        else {
            return ResolvedQualityCatalog {
                selected_unavailable: None,
                candidates: Vec::new(),
                measured_candidate_outputs: Vec::new(),
            };
        };
        let identities = (
            Self::plan_source_identity(file),
            Self::plan_source_identity(&snapshot.file),
        );
        if !matches!(identities, (Ok(expected), Ok(actual)) if expected == actual)
            || file.size != snapshot.file.size
            || file.mtime != snapshot.file.mtime
        {
            return ResolvedQualityCatalog {
                selected_unavailable: None,
                candidates: Vec::new(),
                measured_candidate_outputs: Vec::new(),
            };
        }
        self.quality_catalog_from_snapshot_progress(
            &snapshot,
            caps,
            audio,
            audio_offset_ms,
            subtitle,
            presentation,
            retained_copy,
            retained_audio,
            retained_claim,
            None,
            None,
        )
        .await
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn quality_candidates_from_snapshot_progress(
        &self,
        snapshot: &plurx_core::store::PlaybackPlanningSnapshot,
        caps: &plurx_core::playback::DeviceCaps,
        audio: Option<i64>,
        audio_offset_ms: i64,
        subtitle: Option<i64>,
        presentation: Presentation,
        retained_copy: Option<(bool, bool, bool)>,
        progress: Option<&std::sync::Mutex<Vec<QualityCandidate>>>,
        selected: Option<&QualityCandidate>,
    ) -> Vec<QualityCandidate> {
        let resolved = self
            .quality_catalog_from_snapshot_progress(
                snapshot,
                caps,
                audio,
                audio_offset_ms,
                subtitle,
                presentation,
                retained_copy,
                None,
                None,
                progress,
                selected,
            )
            .await;
        drop(resolved.measured_candidate_outputs);
        resolved.candidates
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn quality_catalog_from_snapshot_progress(
        &self,
        snapshot: &plurx_core::store::PlaybackPlanningSnapshot,
        caps: &plurx_core::playback::DeviceCaps,
        audio: Option<i64>,
        audio_offset_ms: i64,
        subtitle: Option<i64>,
        presentation: Presentation,
        retained_copy: Option<(bool, bool, bool)>,
        retained_audio: Option<&plurx_core::playback::audio::AudioDelivery>,
        retained_claim: Option<&plurx_core::playback::audio::AudioClaim>,
        progress: Option<&std::sync::Mutex<Vec<QualityCandidate>>>,
        selected: Option<&QualityCandidate>,
    ) -> ResolvedQualityCatalog {
        let mut catalog_file = snapshot.file.clone();
        catalog_file.audio_offset_ms = audio_offset_ms;
        let file = &catalog_file;
        let reorder_frames = Self::vod_reorder_from_snapshot(snapshot);
        let audio = Self::candidate_audio_from_snapshot(snapshot, audio);
        let probe: serde_json::Value = match snapshot.probe_json.as_deref() {
            Some(encoded) => match serde_json::from_str(encoded) {
                Ok(probe) => probe,
                Err(_) => {
                    return ResolvedQualityCatalog {
                        selected_unavailable: None,
                        candidates: Vec::new(),
                        measured_candidate_outputs: Vec::new(),
                    }
                }
            },
            None => Self::catalog_plan_probe(file),
        };
        let source_facts = Self::quality_facts_from_probe(file, &probe);
        let Ok(audio_claim) = plurx_core::playback::audio::AudioClaim::from_caps(caps) else {
            return ResolvedQualityCatalog {
                selected_unavailable: None,
                candidates: Vec::new(),
                measured_candidate_outputs: Vec::new(),
            };
        };
        let audio_claim = retained_claim.cloned().or(audio_claim);
        let preference = snapshot
            .settings
            .get(keys::HWACCEL)
            .map(String::as_str)
            .unwrap_or_default();
        let (requested_mode, requested_quality, _) = super::normalize_rate_control_request(
            snapshot
                .settings
                .get(keys::TRANSCODE_RATE_MODE)
                .map(String::as_str),
            snapshot
                .settings
                .get(keys::TRANSCODE_QUALITY)
                .map(String::as_str),
        );
        let rate_control = RateControlSnapshot {
            requested_mode,
            requested_quality,
            quality_rc: self.rate_control_snapshot().quality_rc,
        };
        let coded_rate = source_facts
            .as_ref()
            .and_then(|facts| facts.frame_rate().value())
            .map(|rate| (rate.numerator(), rate.denominator()));
        let mut result = Vec::new();
        let mut profile = plurx_core::playback::DeviceProfile::from_caps_v2(caps);
        profile.retain_applicable_learned_limits(crate::media_sessions::unix_ms());
        let mut node =
            plurx_core::playback::RenderCaps::strip_only(crate::ffmpeg::has_dovi_rpu().await);
        node.dolby_vision_convert = plurx_core::store::stored_switch(
            snapshot.settings.get(keys::DV_CONVERT).map(String::as_str),
            true,
        );
        let copy_decision = plurx_core::playback::decide(file, &profile, &node);
        let (copy_audio, copy_dv, copy_conversion) = retained_copy.unwrap_or((
            copy_decision.transcode_audio,
            copy_decision.preserve_dolby_vision,
            copy_decision.convert_dolby_vision,
        ));
        let copy_possible = copy_decision.method != plurx_core::playback::PlaybackMethod::Transcode
            && subtitle.is_none()
            && selected.is_none_or(|candidate| candidate.route != CandidateRoute::Encode);
        let mut selected_unavailable = None;
        let selected_copy =
            selected.is_some_and(|candidate| candidate.route == CandidateRoute::Remux);
        let copy_engine = if copy_possible {
            match crate::ffmpeg::EncodedExecutable::capture().await {
                Ok(executable) => Some(executable),
                Err(error) => {
                    if selected_copy {
                        selected_unavailable = Some(error);
                    }
                    None
                }
            }
        } else {
            None
        };
        let copy_runtime_engine = if copy_possible {
            match crate::ffmpeg::EncodedEngine::capture(None).await {
                Ok(engine) => Some(engine),
                Err(error) => {
                    if selected_copy {
                        selected_unavailable = Some(error);
                    }
                    None
                }
            }
        } else {
            None
        };
        let copy_source = match crate::fragment_index_cluster::open_source_fence(file, None).await {
            Ok(source) => Some(source),
            Err(error) => {
                if selected_copy {
                    selected_unavailable = Some(error);
                }
                None
            }
        };
        if let (Some(width), Some(height)) = (
            file.width.and_then(|value| u32::try_from(value).ok()),
            file.height.and_then(|value| u32::try_from(value).ok()),
        ) {
            let transfer = match transcode::routing_hdr(file) {
                Some("hlg") => plurx_core::playback::Transfer::Hlg,
                Some("hdr10" | "hdr10plus" | "dolby_vision") => plurx_core::playback::Transfer::Pq,
                _ => plurx_core::playback::Transfer::Sdr,
            };
            let compatible = copy_decision.method
                != plurx_core::playback::PlaybackMethod::Transcode
                && copy_source.is_some()
                && copy_engine.is_some()
                && copy_runtime_engine.is_some()
                && subtitle.is_none()
                && caps.video.iter().any(|entry| {
                    file.video_codec.as_deref() == Some(entry.codec.as_str())
                        && entry.geometry_admission(width, height, coded_rate) != Some(false)
                        && (entry.max_frame_rate.is_none() || coded_rate.is_some())
                        && entry.max_bitrate_bps.is_none_or(|ceiling| {
                            file.bitrate.is_some_and(|bitrate| bitrate <= ceiling)
                        })
                        && entry.present.contains(&transfer)
                        && file.video_profile.as_ref().is_none_or(|profile| {
                            entry.profiles.is_empty()
                                || entry.profiles.iter().any(|claim| {
                                    normalized_profile(claim) == normalized_profile(profile)
                                })
                        })
                });
            if compatible {
                let recipe_digest = copy_candidate_recipe_digest(
                    file,
                    audio,
                    audio_offset_ms,
                    subtitle,
                    (copy_audio, copy_dv, copy_conversion),
                    copy_source.as_ref().map(|source| source.object_version()),
                    copy_engine.as_ref().map(|engine| engine.digest.as_str()),
                    copy_runtime_engine
                        .as_ref()
                        .map(|engine| engine.digest.as_str()),
                    (width, height),
                );
                let candidate = QualityCandidate {
                    id: CandidateId::for_recipe_digest(recipe_digest),
                    recipe_digest,
                    route: CandidateRoute::Remux,
                    normalized_geometry: true,
                    width,
                    height,
                    target_height: height,
                    average_bps: file.bitrate.and_then(|value| u64::try_from(value).ok()),
                    peak_bps: None,
                    grade: copy_candidate_grade(file),
                    decoder_compatible: true,
                    complete_cache: false,
                    sustainable: true,
                };
                if let Some(progress) = progress {
                    progress
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push(candidate.clone());
                }
                result.push(candidate);
            }
        }

        let source_height = source_facts
            .as_ref()
            .and_then(|facts| match facts.rotation_degrees()? {
                90 | 270 => {
                    let sar = facts.sample_aspect_ratio()?;
                    i64::try_from(
                        u64::from(facts.width()?).checked_mul(u64::from(sar.numerator()))?
                            / u64::from(sar.denominator()),
                    )
                    .ok()
                }
                0 | 180 => facts.height().map(i64::from),
                _ => None,
            })
            .or(file.height);
        let heights = candidate_heights(source_height);
        let legacy_geometry_known = source_facts.as_ref().is_some_and(|facts| {
            facts.normalization_transform_known()
                && facts.rotation_degrees() == Some(0)
                && facts
                    .sample_aspect_ratio()
                    .is_some_and(|sar| sar.numerator() == sar.denominator())
        });
        for (height, hdr_requested, normalized_geometry) in heights.into_iter().flat_map(|height| {
            [false, true].into_iter().flat_map(move |hdr| {
                [true, false]
                    .into_iter()
                    .map(move |normalized| (height, hdr, normalized))
            })
        }) {
            if selected.is_some_and(|candidate| {
                candidate.route != CandidateRoute::Encode
                    || i64::from(candidate.target_height) != height
                    || (candidate.grade == OutputGrade::Hdr10) != hdr_requested
                    || candidate.normalized_geometry != normalized_geometry
            }) {
                continue;
            }
            if !normalized_geometry && !legacy_geometry_known {
                continue;
            }
            let (encoder, grade) = match self
                .encoder_and_grade_for_with_preference(
                    file,
                    hdr_requested,
                    height,
                    subtitle.is_some(),
                    preference,
                )
                .await
            {
                Ok(resolved) => resolved,
                Err(error) => {
                    if selected.is_some() {
                        selected_unavailable = Some(error);
                    }
                    continue;
                }
            };
            if hdr_requested && grade != OutputGrade::Hdr10 {
                continue;
            }
            let mut options = self.live_lookup_options(
                rate_control,
                encoder,
                file,
                height,
                0.0,
                audio,
                subtitle.map(|subtitle_index| transcode::SubtitleBurn {
                    subtitle_index,
                    bitmap: file
                        .subtitle_streams
                        .get(subtitle_index as usize)
                        .is_some_and(|stream| {
                            plurx_core::tracks::is_bitmap_subtitle(&stream.codec)
                        }),
                }),
                None,
                grade,
            );
            options.normalized_geometry = normalized_geometry;
            options = match self.candidate_audio_options(
                file,
                audio_claim.as_ref(),
                retained_audio,
                presentation,
                options,
            ) {
                Ok(options) => options,
                Err(_) => continue,
            };
            if normalized_geometry && grade == OutputGrade::Sdr && height == 1440 {
                let profile = transcode::AutoQualityRateProfile::H264Sdr1440P30V1;
                options.auto_quality_rate_profile = Some(profile);
                options.video_bitrate_kbps = profile.video_bitrate_kbps();
                options.effective_rate_control = EffectiveRateControl::Vbr;
            }
            let Some(facts) = source_facts.as_ref() else {
                continue;
            };
            let Ok(plan) = self.resolve_movie_plan_with_facts(
                file,
                &options,
                encoder,
                facts,
                &AttemptRestrictions::none(),
            ) else {
                continue;
            };
            let contract = plan.output_contract();
            let (Some(width), Some(height_px)) =
                (contract.effective_width(), contract.effective_height())
            else {
                continue;
            };
            if height == 1440 && height_px <= 1080 {
                continue;
            }
            let rate = contract
                .normalized_geometry()
                .and_then(|geometry| geometry.frame_rate)
                .or_else(|| {
                    source_facts
                        .as_ref()
                        .and_then(|facts| facts.frame_rate().value())
                })
                .map(|rate| (rate.numerator(), rate.denominator()));
            let decoder_compatible = caps.video.iter().any(|entry| {
                entry.codec == contract.output_codec()
                    && contract.output_profile().is_none_or(|profile| {
                        entry.profiles.is_empty()
                            || entry.profiles.iter().any(|value| {
                                normalized_profile(value) == normalized_profile(profile)
                            })
                    })
                    && entry.geometry_admission(width, height_px, rate) != Some(false)
                    && (entry.max_frame_rate.is_none() || rate.is_some())
                    && entry.max_bitrate_bps.is_none_or(|ceiling| {
                        let audio = if file.audio_streams.is_empty() {
                            0
                        } else {
                            u64::from(plan.options().audio_bitrate_kbps) * 1000
                        };
                        u64::from(plan.options().video_bitrate_kbps) * 1500 + audio
                            <= u64::try_from(ceiling).unwrap_or(0)
                    })
                    && entry.present.contains(&if grade == OutputGrade::Hdr10 {
                        plurx_core::playback::Transfer::Pq
                    } else {
                        plurx_core::playback::Transfer::Sdr
                    })
            });
            let recipe_digest =
                match self.candidate_recipe_digest(&plan, presentation, reorder_frames) {
                    Ok(digest) => digest,
                    Err(error) => {
                        if selected.is_some() {
                            selected_unavailable = Some(error);
                        }
                        continue;
                    }
                };
            let complete_cache = match presentation {
                Presentation::Live => self.verified_cache_hit(&plan).await,
                Presentation::Vod => {
                    self.candidate_complete_vod_cache(
                        file.id,
                        &plan,
                        copy_source.as_ref().map(|source| source.object_version()),
                        reorder_frames,
                    )
                    .await
                }
            };
            // An unnormalized route is offered only when its exact legacy
            // bytes already exist and passed the same integrity/source checks.
            if !normalized_geometry && !complete_cache {
                continue;
            }
            let sustainable = self
                .candidate_production_proof(recipe_digest)
                .is_some_and(|speed| speed >= 1150);
            let audio_budget = if file.audio_streams.is_empty() {
                0
            } else {
                plan.options().audio_bitrate_kbps
            };
            let average_bps =
                (u64::from(plan.options().video_bitrate_kbps) + u64::from(audio_budget)) * 1000;
            let peak_bps = u64::from(plan.options().video_bitrate_kbps) * 1500
                + u64::from(audio_budget) * 1000;
            let candidate = QualityCandidate {
                id: CandidateId::for_recipe_digest(recipe_digest),
                recipe_digest,
                route: CandidateRoute::Encode,
                normalized_geometry,
                width,
                height: height_px,
                target_height: height as u32,
                average_bps: Some(average_bps),
                peak_bps: Some(peak_bps),
                grade,
                decoder_compatible,
                complete_cache,
                sustainable,
            };
            if let Some(progress) = progress {
                progress
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(candidate.clone());
            }
            result.push(candidate);
        }
        // The catalog can expose a complete retained measurement, but its
        // serialized numbers never carry authority. Dispatch still resolves
        // the actual recipe and reacquires an exact private artifact.
        let mut measured_candidate_outputs = Vec::new();
        for candidate in &mut result {
            let kind = match candidate.route {
                CandidateRoute::Remux => SessionKind::Copy {
                    aac: copy_audio,
                    preserve_dolby_vision: copy_dv,
                    convert_dolby_vision: copy_conversion,
                },
                CandidateRoute::Encode => SessionKind::Transcode {
                    height: i64::from(candidate.target_height),
                },
                CandidateRoute::Original => continue,
            };
            let request = SessionRequest {
                sdr_master_codecs: None,
                continuous_media: None,
                vod_only: false,
                passive_vod: false,
                finite_bitrate_limit_bps: None,
                // This request probes a row while its catalog is still being
                // built; it is not the dispatch request retaining that catalog.
                quality_catalog: None,
                candidate_context: Some(Box::new(Self::candidate_context(candidate))),
                file_id: file.id,
                playback_id: String::new(),
                request_id: None,
                control_sequence: None,
                automatic: true,
                previous_session_id: None,
                reopen_reason: None,
                kind,
                start_seconds: 0.0,
                audio_index: audio,
                audio_delivery: retained_audio.cloned().or_else(|| {
                    audio_claim.as_ref().map(|claim| {
                        let selected = audio.map_or_else(
                            || file.audio_streams.first(),
                            |index| {
                                file.audio_streams
                                    .iter()
                                    .find(|stream| stream.index == index)
                            },
                        );
                        let route = match candidate.route {
                            CandidateRoute::Remux => {
                                plurx_core::playback::audio::AudioRoute::Progressive
                            }
                            _ => plurx_core::playback::audio::AudioRoute::EncodedVod,
                        };
                        plurx_core::playback::audio::resolve_audio(
                            selected,
                            &claim.profile(),
                            route,
                            audio_offset_ms,
                        )
                    })
                }),
                audio_claim: audio_claim.clone(),
                subtitle_burn: subtitle,
                audio_offset_ms,
                hdr10: candidate.grade == OutputGrade::Hdr10,
                presentation,
                block_budget_secs: None,
                transport: None,
            };
            let proof = if let Some(source) = copy_source.as_ref() {
                self.measured_candidate_cost(candidate, &request, Some(source))
                    .await
            } else {
                None
            };
            if let Some(proof) = proof {
                let measured = proof.public_descriptor();
                candidate.average_bps = Some(measured.average_bps);
                candidate.peak_bps = Some(measured.peak_bps);
                candidate.complete_cache = true;
                if let Some(progress) = progress {
                    let mut progress = progress
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if let Some(emitted) = progress.iter_mut().find(|emitted| {
                        emitted.id == candidate.id
                            && emitted.recipe_digest == candidate.recipe_digest
                    }) {
                        *emitted = candidate.clone();
                    }
                }
                if measured_candidate_outputs.len() < 64 {
                    measured_candidate_outputs.push(measured);
                }
            }
        }
        ResolvedQualityCatalog {
            selected_unavailable,
            candidates: result,
            measured_candidate_outputs,
        }
    }

    pub(crate) fn candidate_audio_from_snapshot(
        snapshot: &plurx_core::store::PlaybackPlanningSnapshot,
        audio: Option<i64>,
    ) -> Option<i64> {
        let mut prefs = plurx_core::tracks::LangPrefs::default();
        if let Some(value) = snapshot
            .settings
            .get(keys::AUDIO_LANG)
            .filter(|value| !value.trim().is_empty())
        {
            prefs.audio_lang = value.trim().to_owned();
        }
        if let Some(value) = snapshot
            .settings
            .get(keys::SUB_LANG)
            .filter(|value| !value.trim().is_empty())
        {
            prefs.sub_lang = value.trim().to_owned();
        }
        if let Some(value) = snapshot.settings.get(keys::SUB_MODE) {
            prefs.sub_mode = plurx_core::tracks::SubMode::parse(value.trim());
        }
        Self::select_tracks_with_prefs(&snapshot.file, audio, None, &prefs, false).audio_index
    }

    pub(crate) async fn candidate_audio_index(
        &self,
        file: &plurx_core::domain::MediaFile,
        audio: Option<i64>,
    ) -> Option<i64> {
        self.select_tracks(file, audio, None, false)
            .await
            .audio_index
    }

    pub(crate) async fn quality_display_aspect(
        &self,
        file: &plurx_core::domain::MediaFile,
    ) -> Option<plurx_core::playback::geometry::DisplayAspect> {
        let facts = self.quality_source_facts(file).await?;
        facts
            .normalization_transform_known()
            .then(|| facts.displayed_aspect())
            .flatten()
    }

    pub(crate) async fn quality_source_facts(
        &self,
        file: &plurx_core::domain::MediaFile,
    ) -> Option<DecodeFacts> {
        let probe: serde_json::Value =
            serde_json::from_str(&self.store.get_file_probe_json(file.id).await.ok()??).ok()?;
        Self::quality_facts_from_probe(file, &probe)
    }

    pub(crate) fn quality_facts_from_snapshot(
        snapshot: &plurx_core::store::PlaybackPlanningSnapshot,
    ) -> Option<DecodeFacts> {
        let probe = snapshot
            .probe_json
            .as_deref()
            .and_then(|encoded| serde_json::from_str(encoded).ok())
            .unwrap_or_else(|| Self::catalog_plan_probe(&snapshot.file));
        Self::quality_facts_from_probe(&snapshot.file, &probe)
    }

    pub(super) fn quality_facts_from_probe(
        file: &plurx_core::domain::MediaFile,
        probe: &serde_json::Value,
    ) -> Option<DecodeFacts> {
        let index = crate::decode_facts::absolute_video_ordinal(probe, 0)?;
        let catalog = DecodeCatalogMetadata::from_media_file(file).ok()?;
        crate::decode_facts::legacy_ordinal_facts(
            probe,
            Self::plan_source_identity(file).ok()?,
            index,
            Some(&catalog),
        )
        .ok()
    }

    #[cfg(test)]
    pub(crate) async fn restore_candidate_context(
        &self,
        envelope: &mut crate::media_sessions::RemoteStartRequest,
    ) -> Result<(), CandidateRestoreError> {
        self.restore_candidate_context_with_deadline(
            envelope,
            crate::media_pool::create_stage_deadline(Duration::from_secs(2)),
        )
        .await
    }

    /// Ingress and takeover retain their original deadline even before the
    /// startup-budget task-local scope is installed. Compatibility tests can
    /// still use the existing bounded wrapper above.
    pub(crate) async fn restore_candidate_context_with_deadline(
        &self,
        envelope: &mut crate::media_sessions::RemoteStartRequest,
        deadline: tokio::time::Instant,
    ) -> Result<(), CandidateRestoreError> {
        let Some(id) = envelope.candidate_id else {
            if envelope
                .request
                .continuous_media
                .as_ref()
                .is_some_and(|media| media.autonomous_companion.is_some())
            {
                return Err(CandidateRestoreError::Incompatible(
                    "autonomous family primary catalog identity missing".to_owned(),
                ));
            }
            return Ok(());
        };
        let catalog = envelope.candidate_catalog.as_ref().ok_or_else(|| {
            CandidateRestoreError::Incompatible("candidate canonical evidence missing".to_owned())
        })?;
        let restore_deadline = deadline.min(crate::media_pool::create_stage_deadline(
            Duration::from_secs(2),
        ));
        let context = self
            .restore_catalog_context(
                &envelope.request,
                catalog,
                id,
                envelope.source_size,
                envelope.source_mtime,
                envelope.decoder_caps.as_ref(),
                restore_deadline,
            )
            .await?;
        let companion_context = match envelope
            .request
            .continuous_media
            .as_ref()
            .and_then(|media| media.autonomous_companion)
        {
            Some(companion_id) => Some(Box::new(
                self.restore_continuous_companion_context(
                    &envelope.request,
                    &context,
                    companion_id,
                    restore_deadline,
                )
                .await?,
            )),
            None => None,
        };
        if let Some(media) = envelope.request.continuous_media.as_mut() {
            media.companion_context = companion_context;
        }
        envelope.request.candidate_context = Some(Box::new(context));
        Ok(())
    }

    /// An autonomous continuous family re-derives its one companion rung from
    /// the primary's held planning snapshot and canonical caps, scoped to the
    /// exact companion recipe the owner bound. Persistence alone grants nothing.
    async fn restore_continuous_companion_context(
        &self,
        request: &SessionRequest,
        primary: &CandidateExecutionContext,
        companion_id: CandidateId,
        deadline: tokio::time::Instant,
    ) -> Result<ContinuousCompanionContext, CandidateRestoreError> {
        let incompatible = |message: &str| CandidateRestoreError::Incompatible(message.to_owned());
        let selected_companion = request
            .continuous_media
            .as_ref()
            .and_then(|media| media.companion_catalog.as_deref())
            .filter(|row| row.id == companion_id && row.identity_matches())
            .ok_or_else(|| incompatible("continuous companion canonical evidence missing"))?;
        let (Some(planning), Some(canonical_caps)) = (
            primary.planning_snapshot.as_ref(),
            primary.canonical_caps.as_ref(),
        ) else {
            return Err(incompatible(
                "continuous companion planning evidence missing",
            ));
        };
        let retained_copy = match request.kind {
            SessionKind::Copy {
                aac,
                preserve_dolby_vision,
                convert_dolby_vision,
            } => Some((aac, preserve_dolby_vision, convert_dolby_vision)),
            SessionKind::Transcode { .. } => None,
        };
        let resolved = tokio::time::timeout_at(
            deadline,
            self.quality_catalog_from_snapshot_progress(
                planning,
                canonical_caps,
                request.audio_index,
                request.audio_offset_ms,
                request.subtitle_burn,
                request.presentation,
                retained_copy,
                request.audio_delivery.as_ref(),
                request.audio_claim.as_ref(),
                None,
                Some(selected_companion),
            ),
        )
        .await
        .map_err(|_| CandidateRestoreError::Unavailable("candidate restoration deadline".into()))?;
        if let Some(error) = resolved.selected_unavailable {
            return Err(CandidateRestoreError::Unavailable(error));
        }
        let companion = resolved
            .candidates
            .iter()
            .find(|row| {
                row.id == companion_id
                    && row.id != primary.candidate_id
                    && row.decoder_compatible
                    && row.route == CandidateRoute::Encode
                    && row.normalized_geometry
                    && row.grade == OutputGrade::Sdr
                    && row.recipe_digest == selected_companion.recipe_digest
                    && row.target_height != primary.selected_candidate.target_height
            })
            .ok_or_else(|| incompatible("autonomous companion recipe or decoder changed"))?;
        let mut context = Self::candidate_context(companion);
        context.canonical_caps = Some(canonical_caps.clone());
        context.planning_binding = primary.planning_binding.clone();
        context.planning_snapshot = Some(Arc::clone(planning));
        Ok(ContinuousCompanionContext {
            height: i64::from(companion.target_height),
            candidate: context,
        })
    }

    /// Worker envelopes and claimed background intents re-enter the same
    /// catalog authority. Neither persistence nor an id alone grants dispatch.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn restore_catalog_context(
        &self,
        request: &SessionRequest,
        catalog: &crate::media_sessions::CandidateCatalogContext,
        id: CandidateId,
        source_size: i64,
        source_mtime: i64,
        decoder: Option<&crate::playback_control::DecoderCapsSnapshot>,
        deadline: tokio::time::Instant,
    ) -> Result<CandidateExecutionContext, CandidateRestoreError> {
        if tokio::time::Instant::now() >= deadline {
            return Err(CandidateRestoreError::Unavailable(
                "candidate restoration deadline".into(),
            ));
        }
        tokio::time::timeout_at(deadline, async {
            let retained_copy = match request.kind {
                SessionKind::Copy {
                    aac,
                    preserve_dolby_vision,
                    convert_dolby_vision,
                } => Some((aac, preserve_dolby_vision, convert_dolby_vision)),
                SessionKind::Transcode { .. } => None,
            };
            let mut query = crate::media_pool::QualityCatalogRequest {
                file_id: request.file_id,
                source_size,
                source_mtime,
                caps: catalog.caps.clone(),
                audio_index: request.audio_index,
                audio_offset_ms: request.audio_offset_ms,
                audio_claim: request.audio_claim.clone(),
                audio_delivery: request.audio_delivery.clone(),
                subtitle_burn: request.subtitle_burn,
                presentation: request.presentation,
                copy_contract: retained_copy,
            };
            query
                .validate()
                .map_err(|error| CandidateRestoreError::Incompatible(format!("{error:?}")))?;
            if let Some(decoder) = decoder {
                query.caps.video = decoder.device_caps().video;
                query
                    .validate()
                    .map_err(|error| CandidateRestoreError::Incompatible(format!("{error:?}")))?;
            }
            let planning = self
                .store
                .playback_planning_snapshot(request.file_id, &QUALITY_PLANNING_KEYS)
                .await
                .map_err(|error| CandidateRestoreError::Unavailable(error.to_string()))?
                .ok_or_else(|| {
                    CandidateRestoreError::Incompatible("candidate source missing".to_owned())
                })?;
            if catalog.binding != crate::media_pool::PlanningBinding::from_snapshot(&planning) {
                return Err(CandidateRestoreError::Incompatible(
                    "candidate source or settings changed".to_owned(),
                ));
            }
            let file = &planning.file;
            if file.size != source_size || file.mtime != source_mtime {
                return Err(CandidateRestoreError::Incompatible(
                    "candidate source changed".to_owned(),
                ));
            }
            let canonical_caps = query.caps;
            let selected = &catalog.candidate;
            if selected.id != id || !selected.identity_matches() {
                return Err(CandidateRestoreError::Incompatible(
                    "candidate descriptor identity mismatch".to_owned(),
                ));
            }
            match request.kind {
                SessionKind::Transcode { height }
                    if selected.route == CandidateRoute::Encode
                        && height == i64::from(selected.target_height)
                        && request.hdr10 == (selected.grade == OutputGrade::Hdr10) => {}
                SessionKind::Copy { .. } if selected.route == CandidateRoute::Remux => {}
                _ => {
                    return Err(CandidateRestoreError::Incompatible(
                        "candidate delivery mismatch".into(),
                    ))
                }
            }
            let candidates = self
                .quality_catalog_from_snapshot_progress(
                    &planning,
                    &canonical_caps,
                    request.audio_index,
                    request.audio_offset_ms,
                    request.subtitle_burn,
                    request.presentation,
                    retained_copy,
                    request.audio_delivery.as_ref(),
                    request.audio_claim.as_ref(),
                    None,
                    Some(selected),
                )
                .await;
            if let Some(error) = candidates.selected_unavailable {
                return Err(CandidateRestoreError::Unavailable(error));
            }
            let candidate = candidates
                .candidates
                .iter()
                .find(|candidate| {
                    candidate.id == id
                        && candidate.decoder_compatible
                        && candidate.recipe_digest == selected.recipe_digest
                        && candidate.route == selected.route
                        && candidate.grade == selected.grade
                        && candidate.width == selected.width
                        && candidate.height == selected.height
                        && candidate.target_height == selected.target_height
                        && candidate.normalized_geometry == selected.normalized_geometry
                })
                .ok_or_else(|| {
                    CandidateRestoreError::Incompatible(
                        "candidate recipe or worker changed".to_owned(),
                    )
                })?;
            match request.kind {
                SessionKind::Transcode { height }
                    if candidate.route == CandidateRoute::Encode
                        && height == i64::from(candidate.target_height)
                        && request.hdr10 == (candidate.grade == OutputGrade::Hdr10) => {}
                SessionKind::Copy { .. } if candidate.route != CandidateRoute::Encode => {}
                _ => {
                    return Err(CandidateRestoreError::Incompatible(
                        "candidate delivery mismatch".to_owned(),
                    ))
                }
            }
            let mut context = Self::candidate_context(candidate);
            context.canonical_caps = Some(canonical_caps);
            context.planning_binding =
                Some(crate::media_pool::PlanningBinding::from_snapshot(&planning));
            context.planning_snapshot = Some(Arc::new(planning));
            Ok(context)
        })
        .await
        .map_err(|_| CandidateRestoreError::Unavailable("candidate restoration deadline".into()))?
    }

    pub(super) fn candidate_audio_options(
        &self,
        file: &plurx_core::domain::MediaFile,
        claim: Option<&plurx_core::playback::audio::AudioClaim>,
        retained: Option<&plurx_core::playback::audio::AudioDelivery>,
        presentation: Presentation,
        options: TranscodeOptions,
    ) -> Result<TranscodeOptions, String> {
        if presentation == Presentation::Vod {
            Self::encoded_audio_options(file, options.audio_index, claim, retained, options)
        } else {
            Ok(self.rolling_start_audio_options(file, options, claim, retained))
        }
    }

    /// Bind only a current ordinary eligible row to the validated restored
    /// context. Costs/cache availability are advisory; recipe/source/owner are
    /// exact. Failure leaves the context unchanged and forbids fresh reuse.
    pub(crate) fn bind_prepared_candidate_owner(
        context: &mut CandidateExecutionContext,
        reserved_owner: &str,
        eligible: Option<&crate::media_pool::WorkerQualityCandidate>,
    ) -> bool {
        let Some(entry) = eligible else {
            return false;
        };
        let selected = &context.selected_candidate;
        let actual = &entry.candidate;
        if reserved_owner.is_empty()
            || entry.node_id != reserved_owner
            || !entry.dispatch_supported
            || entry.partial
            || !actual.decoder_compatible
            || !actual.identity_matches()
            || context.planning_binding.is_none()
            || context.planning_binding != entry.binding
            || context
                .owner_node_id
                .as_ref()
                .is_some_and(|current| current != &entry.node_id)
            || context.candidate_id != actual.id
            || context.recipe_digest != actual.recipe_digest
            || selected.id != actual.id
            || selected.recipe_digest != actual.recipe_digest
            || selected.route != actual.route
            || selected.grade != actual.grade
            || selected.width != actual.width
            || selected.height != actual.height
            || selected.target_height != actual.target_height
            || selected.normalized_geometry != actual.normalized_geometry
        {
            return false;
        }
        context.owner_node_id = Some(entry.node_id.clone());
        true
    }

    pub(crate) fn candidate_context(candidate: &QualityCandidate) -> CandidateExecutionContext {
        CandidateExecutionContext {
            retained_output: None,
            canonical_caps: None,
            selected_candidate: candidate.clone(),
            planning_binding: None,
            planning_snapshot: None,
            owner_node_id: None,
            candidate_id: candidate.id,
            recipe_digest: candidate.recipe_digest,
            normalized_geometry: candidate.normalized_geometry,
            grade: candidate.grade,
            profile: (candidate.normalized_geometry
                && candidate.grade == OutputGrade::Sdr
                && candidate.target_height == 1440)
                .then_some(transcode::AutoQualityRateProfile::H264Sdr1440P30V1),
        }
    }

    /// Main's saved operator choice, read only from the held atomic query.
    pub(crate) fn vod_reorder_from_snapshot(
        snapshot: &plurx_core::store::PlaybackPlanningSnapshot,
    ) -> bool {
        snapshot
            .settings
            .get("playback.vod_reorder_frames")
            .map(String::as_str)
            == Some("2")
    }
}

fn normalized_profile(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

fn candidate_heights(source_height: Option<i64>) -> Vec<i64> {
    let mut heights: Vec<i64> = ladder(source_height)
        .into_iter()
        .map(|rung| rung.height)
        .collect();
    if let Some(source_height) = source_height.filter(|height| *height >= MIN_HEIGHT) {
        let ceiling = source_height.min(MAX_HEIGHT);
        heights.extend([1440, 2160].into_iter().filter(|height| *height <= ceiling));
        heights.push(ceiling);
    }
    heights.sort_unstable();
    heights.dedup();
    heights
}

/// Every setting a playback plan or a rolling start reads, fetched in the one
/// statement that also returns the file, its probe and the planning
/// generation. One list on purpose: every snapshot that reaches a
/// `CandidateExecutionContext` is fetched with it, so a start bound to a
/// candidate sees exactly the keys an unbound start reads instead of reading
/// some of them as unset. Widening it changes no planning identity:
/// `PlanningBinding::from_snapshot` hashes the file, probe, reorder flag and
/// generation, never this map.
pub(crate) const QUALITY_PLANNING_KEYS: [&str; 22] = [
    keys::HWACCEL,
    keys::DV_CONVERT,
    keys::AUDIO_LANG,
    keys::SUB_LANG,
    keys::SUB_MODE,
    keys::PLAYBACK_DISPLAY_AWARE_AUTO,
    keys::PLAYBACK_CONTROL_PROTOCOL_V1,
    keys::TRANSCODE_RATE_MODE,
    keys::TRANSCODE_QUALITY,
    "playback.vod_reorder_frames",
    // Not a planning input: carried in the same committed read so the create
    // freezes the session's SDR master shape without a second Store read
    // (`SessionRequest::sdr_master_codecs`).
    keys::PLAYBACK_SDR_MASTER_CODECS,
    // The rolling start path (D4, main-merge defects 2026-10-04): retention
    // budget, input pacing, admission pools, content-aware lookup, the three
    // scratch/ahead limits, and the two complete-output switches.
    keys::CACHE_MAX_GB,
    keys::HLS_READRATE,
    keys::HLS_BURST_SECS,
    keys::SW_POOL_THREADS,
    keys::MAX_HW_SESSIONS,
    keys::CONTENT_AWARE_ENCODING,
    keys::HLS_AHEAD_MAX_SECS,
    keys::HLS_AHEAD_MAX_BYTES,
    keys::HLS_SCRATCH_MAX_BYTES,
    keys::VOD_OUTPUT_PREPARATION,
    keys::VOD_ROLLING_RETENTION,
];

#[cfg(test)]
mod tests {
    #[test]
    fn candidate_catalog_keeps_qualified_2160_and_scope_source_recipes() {
        assert_eq!(
            super::candidate_heights(Some(2160)),
            vec![144, 240, 360, 480, 720, 1080, 1440, 2160]
        );
        assert_eq!(
            super::candidate_heights(Some(1600)),
            vec![144, 240, 360, 480, 720, 1080, 1440, 1600]
        );
        assert_eq!(
            super::candidate_heights(Some(1080)),
            vec![144, 240, 360, 480, 720, 1080]
        );
        assert_eq!(super::candidate_heights(Some(4320)).last(), Some(&2160));
    }
}

#[cfg(test)]
mod snapshot_catalog_regression {
    use super::*;
    #[tokio::test]
    async fn encoded_vod_reorder_choice_is_exact_across_catalog_evidence_and_restore() {
        use plurx_core::{
            domain::{ItemKind, LibraryKind, NewItem, NewLibrary},
            store::SqliteStore,
        };
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let library = store
            .create_library(&NewLibrary {
                name: "reorder".into(),
                kind: LibraryKind::Movies,
                paths: vec![],
                anime: false,
            })
            .await
            .expect("library");
        let item = store
            .insert_item(&NewItem {
                library_id: library.id,
                kind: ItemKind::Movie,
                parent_id: None,
                title: "synthetic".into(),
                year: None,
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("item");
        // HEVC source/H264 client cannot take the copy branch that attests an
        // executable. Only the existing bounded -bsfs catalog probe is needed.
        let probe = serde_json::json!({"format":{"duration":"10.0"}, "streams":[{
            "index":0, "codec_type":"video", "codec_name":"hevc", "profile":"Main",
            "width":1280, "height":720, "sample_aspect_ratio":"1:1", "pix_fmt":"yuv420p",
            "avg_frame_rate":"30/1", "r_frame_rate":"30/1", "color_transfer":"bt709",
            "side_data_list":[{"side_data_type":"Display Matrix", "rotation":0,
                "displaymatrix":"00000000: 65536 0 0\n00000001: 0 65536 0\n00000002: 0 0 1073741824\n"}]
        }]});
        let parsed = plurx_core::scan::probe::parse_probe_json(&probe);
        let id = store
            .upsert_file(item, "/sanitized/reorder.mkv", 1234, 123, &parsed)
            .await
            .expect("file");
        store
            .put_setting(keys::HWACCEL, "software")
            .await
            .expect("preference");
        store
            .put_setting("playback.vod_reorder_frames", "0")
            .await
            .expect("off");
        let off_snapshot = store
            .playback_planning_snapshot(id, &QUALITY_PLANNING_KEYS)
            .await
            .expect("snapshot")
            .expect("source");
        assert_eq!(QUALITY_PLANNING_KEYS.len(), 22);
        assert!(
            QUALITY_PLANNING_KEYS.len() <= 32,
            "one settings statement binds at most 32 keys"
        );
        assert!(QUALITY_PLANNING_KEYS.contains(&keys::PLAYBACK_SDR_MASTER_CODECS));
        assert!(QUALITY_PLANNING_KEYS.contains(&"playback.vod_reorder_frames"));
        assert!(!TranscodeManager::vod_reorder_from_snapshot(&off_snapshot));
        let base = crate::test_tempdir().expect("work");
        let manager = TranscodeManager::new(
            Arc::clone(&store),
            base.path().join("work"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        )
        .with_cache(
            base.path().join("cache"),
            "synthetic-ffmpeg".into(),
            "synthetic".into(),
        )
        .with_decoders(vec!["hevc".into()]);
        let caps: plurx_core::playback::DeviceCaps = serde_json::from_value(serde_json::json!({
            "v":2, "video":[{"codec":"h264", "decode":true, "present":["sdr"]}],
            "audio":["aac"],
            "audio_sinks":[{"codec":"aac", "max_channels":2, "sample_rates_hz":[48000]}],
            "transports":["hls"]
        }))
        .expect("caps");
        let off_rows = manager
            .quality_candidates_from_snapshot_progress(
                &off_snapshot,
                &caps,
                None,
                0,
                None,
                Presentation::Vod,
                None,
                None,
                None,
            )
            .await;
        let off = off_rows
            .iter()
            .find(|row| {
                row.route == CandidateRoute::Encode
                    && row.normalized_geometry
                    && row.target_height == 720
                    && row.grade == OutputGrade::Sdr
            })
            .expect("off encoded row")
            .clone();
        let mut on_snapshot = off_snapshot.clone();
        on_snapshot
            .settings
            .insert("playback.vod_reorder_frames".into(), "2".into());
        // Equal generations/source facts still cannot alias effective choices.
        assert_ne!(
            crate::media_pool::PlanningBinding::from_snapshot(&off_snapshot),
            crate::media_pool::PlanningBinding::from_snapshot(&on_snapshot)
        );
        let on_rows = manager
            .quality_candidates_from_snapshot_progress(
                &on_snapshot,
                &caps,
                None,
                0,
                None,
                Presentation::Vod,
                None,
                None,
                Some(&off),
            )
            .await;
        let on = on_rows
            .iter()
            .find(|row| {
                row.target_height == off.target_height
                    && row.normalized_geometry == off.normalized_geometry
                    && row.grade == off.grade
            })
            .expect("on encoded row")
            .clone();
        assert_ne!(off.recipe_digest, on.recipe_digest);
        assert_ne!(off.id, on.id);
        manager.candidate_production_proofs.record_for_test(
            off.recipe_digest,
            crate::vodencode::ActiveProductionEvidence {
                milli_realtime: 1500,
                active_ms: 2000,
                completed_segments: 2,
                observed_at: std::time::Instant::now(),
            },
        );
        assert_eq!(
            manager.candidate_production_proof(off.recipe_digest),
            Some(1500)
        );
        assert_eq!(manager.candidate_production_proof(on.recipe_digest), None);
        let (measured_off, measured_on) =
            crate::vodserve::retained::test_reorder_candidate_cost_isolation(&off, &on).await;
        assert_eq!(measured_off, off.recipe_digest);
        assert!(
            measured_on.is_none(),
            "the other digest cannot inherit a complete artifact's cost"
        );

        let file = &off_snapshot.file;
        let (encoder, grade) = manager
            .encoder_and_grade_for_with_preference(
                file,
                false,
                i64::from(off.target_height),
                false,
                "software",
            )
            .await
            .expect("software");
        let mut options = manager.live_lookup_options(
            manager.rate_control_snapshot(),
            encoder,
            file,
            i64::from(off.target_height),
            0.0,
            None,
            None,
            None,
            grade,
        );
        options.normalized_geometry = true;
        let claim = plurx_core::playback::audio::AudioClaim::from_caps(&caps).expect("claim");
        options = manager
            .candidate_audio_options(file, claim.as_ref(), None, Presentation::Vod, options)
            .expect("audio");
        let facts = TranscodeManager::quality_facts_from_snapshot(&off_snapshot).expect("facts");
        let plan = manager
            .resolve_movie_plan_with_facts(
                file,
                &options,
                encoder,
                &facts,
                &AttemptRestrictions::none(),
            )
            .expect("plan");
        let context = TranscodeManager::candidate_context(&off);
        assert_eq!(
            manager
                .validate_prepared_candidate_recipe(
                    &plan,
                    Presentation::Vod,
                    TranscodeManager::vod_reorder_from_snapshot(&off_snapshot),
                    &context
                )
                .expect("accepted preparation seam"),
            off.recipe_digest
        );
        assert!(manager
            .validate_prepared_candidate_recipe(&plan, Presentation::Vod, true, &context)
            .is_err());
        assert_eq!(
            manager
                .candidate_recipe_digest(&plan, Presentation::Live, false)
                .expect("rolling"),
            manager
                .candidate_recipe_digest(&plan, Presentation::Live, true)
                .expect("rolling unchanged")
        );
        let mut reopened = off_snapshot.clone();
        reopened.generation += 1;
        assert_ne!(
            crate::media_pool::PlanningBinding::from_snapshot(&off_snapshot),
            crate::media_pool::PlanningBinding::from_snapshot(&reopened)
        );
        let reopened_rows = manager
            .quality_candidates_from_snapshot_progress(
                &reopened,
                &caps,
                None,
                0,
                None,
                Presentation::Vod,
                None,
                None,
                Some(&off),
            )
            .await;
        assert!(reopened_rows
            .iter()
            .any(|row| row.id == off.id && row.recipe_digest == off.recipe_digest));
        for value in ["", "0", "1", " 2", "garbage"] {
            let mut snapshot = off_snapshot.clone();
            snapshot
                .settings
                .insert("playback.vod_reorder_frames".into(), value.into());
            assert!(!TranscodeManager::vod_reorder_from_snapshot(&snapshot));
        }
        let request = SessionRequest {
            sdr_master_codecs: None,
            continuous_media: None,
            vod_only: false,
            passive_vod: false,
            finite_bitrate_limit_bps: None,
            quality_catalog: None,
            candidate_context: None,
            file_id: id,
            playback_id: "reorder".into(),
            request_id: Some("00000000-0000-4000-8000-0000000000f3".into()),
            control_sequence: None,
            automatic: true,
            previous_session_id: None,
            reopen_reason: None,
            kind: SessionKind::Transcode { height: 720 },
            start_seconds: 0.0,
            audio_index: None,
            audio_delivery: None,
            audio_claim: claim,
            subtitle_burn: None,
            audio_offset_ms: 0,
            hdr10: false,
            presentation: Presentation::Vod,
            block_budget_secs: None,
            transport: None,
        };
        let mut envelope = crate::media_sessions::RemoteStartRequest {
            retained_output_receiver: None,
            retained_output: None,
            candidate_catalog: Some(crate::media_sessions::CandidateCatalogContext {
                caps: caps.clone(),
                candidate: off.clone(),
                binding: crate::media_pool::PlanningBinding::from_snapshot(&off_snapshot),
            }),
            candidate_id: Some(off.id),
            presentation_target: None,
            decoder_caps: Some(
                crate::playback_control::DecoderCapsSnapshot::from_device_caps(&caps, 1)
                    .expect("decoder snapshot"),
            ),
            protocol_version: crate::media_pool::PROTOCOL_VERSION,
            incarnation_id: "00000000-0000-4000-8000-0000000000f3".into(),
            principal: plurx_core::playback_principal::PlaybackPrincipal::LocalUser { user_id: 7 },
            source_size: file.size,
            source_mtime: file.mtime,
            typeless_playlist: true,
            library_channel: None,
            request,
        };
        assert!(
            envelope.is_valid(),
            "actual worker envelope bounds apply to the synthetic metadata"
        );
        manager
            .restore_candidate_context(&mut envelope)
            .await
            .expect("same choice restore");
        let held = manager
            .vod_preparation_snapshot(&envelope.request, file)
            .await
            .expect("actual restored carrier");
        assert!(!TranscodeManager::vod_reorder_from_snapshot(&held));
        let encoded = serde_json::to_value(&envelope).expect("strict worker JSON");
        assert!(encoded["request"].get("candidate_context").is_none());
        assert!(encoded["request"].get("quality_catalog").is_none());
        assert!(encoded["request"].get("planning_snapshot").is_none());
        let mut extra_wire_choice = encoded.clone();
        extra_wire_choice["reorder_frames"] = serde_json::json!(true);
        assert!(
            serde_json::from_value::<crate::media_sessions::RemoteStartRequest>(extra_wire_choice)
                .is_err(),
            "the strict worker wire cannot inject a second choice authority"
        );
        let mut wire: crate::media_sessions::RemoteStartRequest =
            serde_json::from_value(encoded).expect("strict restore");
        assert!(
            wire.request.candidate_context.is_none(),
            "serde cannot mint private authority"
        );
        manager
            .restore_candidate_context(&mut wire)
            .await
            .expect("same wire binding reconstructs authority");
        let mut fallback = wire.request.clone();
        fallback
            .candidate_context
            .as_mut()
            .expect("context")
            .planning_snapshot = None;
        assert!(!TranscodeManager::vod_reorder_from_snapshot(
            &manager
                .vod_preparation_snapshot(&fallback, file)
                .await
                .expect("real atomic fallback")
        ));
        let mut mismatched_file = file.clone();
        mismatched_file.mtime += 1;
        assert!(manager
            .vod_preparation_snapshot(&wire.request, &mismatched_file)
            .await
            .is_err());
        let mut unbound = wire.request.clone();
        unbound
            .candidate_context
            .as_mut()
            .expect("context")
            .planning_binding = None;
        assert!(
            manager
                .vod_preparation_snapshot(&unbound, file)
                .await
                .is_err(),
            "held but unbound is not authority"
        );
        unbound
            .candidate_context
            .as_mut()
            .expect("context")
            .planning_snapshot = None;
        assert!(
            manager
                .vod_preparation_snapshot(&unbound, file)
                .await
                .is_err(),
            "absence of both carrier and binding cannot mint candidate authority"
        );
        store
            .put_setting("playback.vod_reorder_frames", "2")
            .await
            .expect("current on");
        assert!(manager
            .restore_candidate_context(&mut wire)
            .await
            .expect_err("stale off binding refuses")
            .is_incompatible());
        assert!(
            manager
                .vod_preparation_snapshot(&fallback, file)
                .await
                .is_err(),
            "fallback cannot replace accepted choice"
        );
        let current = store
            .playback_planning_snapshot(id, &QUALITY_PLANNING_KEYS)
            .await
            .expect("current")
            .expect("source");
        envelope.candidate_id = Some(on.id);
        envelope.candidate_catalog = Some(crate::media_sessions::CandidateCatalogContext {
            caps,
            candidate: on.clone(),
            binding: crate::media_pool::PlanningBinding::from_snapshot(&current),
        });
        envelope.request.candidate_context = None;
        manager
            .restore_candidate_context(&mut envelope)
            .await
            .expect("same on choice restore");
        let held = manager
            .vod_preparation_snapshot(&envelope.request, file)
            .await
            .expect("on carrier");
        assert!(TranscodeManager::vod_reorder_from_snapshot(&held));
        assert_eq!(
            envelope
                .request
                .candidate_context
                .as_ref()
                .expect("restored")
                .recipe_digest,
            on.recipe_digest
        );

        // Extend this still-unexecuted control, rather than relabelling any
        // historical manual pass as automatic authority. All fixtures below
        // are metadata-only: no encoder or media producer is opened.
        use plurx_core::store::background_jobs::*;
        struct CarrierAuthority;
        #[plurx_core::cluster::coordination::cluster_job_async_trait]
        impl plurx_core::cluster::coordination::ClusterJobAuthority for CarrierAuthority {
            async fn may_run_cluster_jobs(&self) -> bool {
                true
            }
        }
        async fn claimed_metadata(
            store: &Arc<dyn Store>,
            file: &plurx_core::domain::MediaFile,
            carrier: Option<serde_json::Value>,
        ) -> (BackgroundJob, crate::background_jobs::ActiveBackgroundJob) {
            let now = crate::media_sessions::unix_ms();
            let id = uuid::Uuid::new_v4().to_string();
            let payload = JobPayload::CopyOutputPrepare {
                copy_output_version: 1,
                file_id: file.id,
                source_generation: "metadata:1".into(),
                source_size: file.size,
                source_mtime: file.mtime,
                source_object_version: "metadata:1".into(),
                policy_generation: "copy_output_v1".into(),
                intent: CopyOutputIntent {
                    target_node_id: "synthetic".into(),
                    audio_index: None,
                    audio_offset_ms: 0,
                    audio_claim: Some(plurx_core::playback::audio::AudioClaim {
                        decoders: vec!["aac".into()],
                        sinks: vec![],
                    }),
                    audio_delivery: plurx_core::playback::audio::AudioDelivery {
                        action: plurx_core::playback::audio::AudioAction::None,
                        downmix: None,
                        reason: "no_audio".into(),
                    },
                    aac: false,
                    preserve_dolby_vision: false,
                    convert_dolby_vision: false,
                    grade: OutputGrade::Sdr,
                    hdr10_requested: false,
                    normalized_geometry: true,
                    profile: None,
                    width: 1280,
                    height: 720,
                    video_identity: "a".repeat(64),
                    pipeline_identity: "b".repeat(64),
                },
                candidate_catalog: carrier,
                scratch_bytes: 4096,
                reason: "recent_demand".into(),
            };
            store
                .enqueue_job(EnqueueJob {
                    id: id.clone(),
                    payload,
                    dedupe_key: id.clone(),
                    priority: 1,
                    not_before_ms: now,
                    now_ms: now,
                    request: JobRequest {
                        scope: "copy_output_prepare".into(),
                        request_id: id.clone(),
                        request_digest: "c".repeat(64),
                        consumer_kind: "copy_output".into(),
                        consumer_ref: file.id.to_string(),
                        target_node_id: Some("synthetic".into()),
                        deadline_ms: None,
                        retain_identity: false,
                    },
                })
                .await
                .expect("metadata enqueue");
            let claimed = match store
                .claim_job(ClaimJob {
                    job_id: id,
                    expected_revision: 0,
                    node_id: "synthetic".into(),
                    boot_id: uuid::Uuid::new_v4().to_string(),
                    claim_id: uuid::Uuid::new_v4().to_string(),
                    kind: JobKind::CopyOutputPrepare,
                    payload_version: 1,
                    now_ms: now,
                    dispatched_at_ms: now,
                })
                .await
                .expect("metadata claim")
            {
                ClaimOutcome::Claimed { job } => *job,
                other => panic!("metadata claim {other:?}"),
            };
            let active = crate::background_jobs::ActiveBackgroundJob::start(
                Arc::clone(store),
                Arc::new(CarrierAuthority),
                claimed.token.clone().expect("token"),
                tokio::time::Instant::now() + Duration::from_secs(10),
                JobKind::CopyOutputPrepare,
            )
            .expect("metadata fence");
            (claimed, active)
        }
        let mut queued = envelope.request.clone();
        queued
            .candidate_context
            .as_mut()
            .expect("actual on context")
            .owner_node_id = Some("synthetic".into());
        let carrier = manager
            .queued_candidate_catalog(&queued, file, "synthetic")
            .await
            .expect("actual enqueue authority")
            .expect("carrier");
        let decoded: crate::media_sessions::CandidateCatalogContext =
            serde_json::from_value(carrier.clone()).expect("strict canonical parser");
        assert_eq!(decoded.candidate.id, on.id);
        assert_eq!(
            decoded.binding,
            crate::media_pool::PlanningBinding::from_snapshot(&current)
        );
        let (job, active) = claimed_metadata(&store, file, Some(carrier.clone())).await;
        assert_eq!(
            TranscodeManager::claimed_preparation_file(
                store.as_ref(),
                file.id,
                file.size,
                file.mtime,
                &active.fence()
            )
            .await
            .expect("matching claimed row")
            .expect("present")
            .id,
            file.id
        );
        let mut restored = queued.clone();
        restored.candidate_context = None;
        assert!(manager
            .restore_queued_candidate(
                &mut restored,
                file,
                Some(&carrier),
                Some((on.id, on.recipe_digest)),
                "synthetic",
                &active.fence(),
                Instant::now() + Duration::from_secs(2)
            )
            .await
            .expect("actual queued restore"));
        assert_eq!(
            restored
                .candidate_context
                .as_ref()
                .expect("restored")
                .recipe_digest,
            on.recipe_digest
        );
        assert!(TranscodeManager::vod_reorder_from_snapshot(
            restored
                .candidate_context
                .as_ref()
                .expect("held authority")
                .planning_snapshot
                .as_ref()
                .expect("snapshot")
        ));
        // A selected Remux descriptor with unavailable actual source must
        // preserve typed availability from THAT enumeration, not a separate
        // preflight. This sanitized metadata fixture has no physical source.
        let mut unavailable_catalog = decoded.clone();
        unavailable_catalog.candidate.route = CandidateRoute::Remux;
        unavailable_catalog.caps.video[0].codec = "hevc".into();
        let mut unavailable_request = queued.clone();
        unavailable_request.candidate_context = None;
        unavailable_request.kind = SessionKind::Copy {
            aac: false,
            preserve_dolby_vision: false,
            convert_dolby_vision: false,
        };
        let unavailable = serde_json::to_value(unavailable_catalog).expect("bounded descriptor");
        let error = manager
            .restore_queued_candidate(
                &mut unavailable_request,
                file,
                Some(&unavailable),
                None,
                "synthetic",
                &active.fence(),
                Instant::now() + Duration::from_secs(2),
            )
            .await
            .expect_err("actual source unavailable retries");
        assert!(!error.is_empty());
        assert!(unavailable_request.candidate_context.is_none());
        let still_claimed = store
            .background_job(&job.id)
            .await
            .expect("readback")
            .expect("row");
        assert_eq!(
            still_claimed.state,
            JobState::Running,
            "unavailability never Stops the claim"
        );
        assert_eq!(still_claimed.failed_attempts, 0);
        assert!(!manager
            .restore_catalog_context(
                &queued,
                &decoded,
                on.id,
                file.size,
                file.mtime,
                None,
                tokio::time::Instant::now() - Duration::from_millis(1)
            )
            .await
            .expect_err("expired inherited deadline")
            .is_incompatible());
        let mut expired_envelope = envelope.clone();
        expired_envelope.request.candidate_context = None;
        assert!(!manager
            .restore_candidate_context_with_deadline(
                &mut expired_envelope,
                tokio::time::Instant::now() - Duration::from_millis(1),
            )
            .await
            .expect_err("entry retains expired ingress deadline")
            .is_incompatible());
        assert!(
            expired_envelope.request.candidate_context.is_none(),
            "expired entry mints no authority"
        );
        manager
            .restore_candidate_context(&mut expired_envelope)
            .await
            .expect("existing bounded test wrapper still restores current authority");
        assert_eq!(
            expired_envelope
                .request
                .candidate_context
                .as_ref()
                .expect("restored")
                .recipe_digest,
            on.recipe_digest
        );
        // Actual source/catalog restoration loses process-private dispatch
        // ownership on the wire. Only current ordinary eligible source authority
        // may restore it, without changing the audio claim or recipe.
        let restored = expired_envelope
            .request
            .candidate_context
            .as_ref()
            .expect("restored")
            .clone();
        assert!(restored.owner_node_id.is_none());
        let eligible = crate::media_pool::WorkerQualityCandidate {
            node_id: "prepared-owner".into(),
            candidate: restored.selected_candidate.clone(),
            binding: restored.planning_binding.clone(),
            partial: false,
            dispatch_supported: true,
        };
        let original_claim = expired_envelope.request.audio_claim.clone();
        let mut bound = restored.clone();
        assert!(TranscodeManager::bind_prepared_candidate_owner(
            &mut bound,
            "prepared-owner",
            Some(&eligible),
        ));
        assert_eq!(bound.owner_node_id.as_deref(), Some("prepared-owner"));
        assert_eq!(bound.selected_candidate, restored.selected_candidate);
        assert_eq!(bound.planning_binding, restored.planning_binding);
        assert_eq!(expired_envelope.request.audio_claim, original_claim);
        let mut refusals = Vec::new();
        let mut wrong_owner = eligible.clone();
        wrong_owner.node_id = "another-owner".into();
        refusals.push(wrong_owner);
        let mut wrong_binding = eligible.clone();
        wrong_binding
            .binding
            .as_mut()
            .expect("actual source binding")
            .generation += 1;
        refusals.push(wrong_binding);
        let mut absent_binding = eligible.clone();
        absent_binding.binding = None;
        refusals.push(absent_binding);
        let mut wrong_candidate = eligible.clone();
        wrong_candidate.candidate.recipe_digest[0] ^= 1;
        refusals.push(wrong_candidate);
        let mut unsupported = eligible.clone();
        unsupported.dispatch_supported = false;
        refusals.push(unsupported);
        let mut partial = eligible.clone();
        partial.partial = true;
        refusals.push(partial);
        let mut incompatible = eligible.clone();
        incompatible.candidate.decoder_compatible = false;
        refusals.push(incompatible);
        for refused in &refusals {
            let mut unchanged = restored.clone();
            assert!(!TranscodeManager::bind_prepared_candidate_owner(
                &mut unchanged,
                "prepared-owner",
                Some(refused),
            ));
            assert!(
                unchanged.owner_node_id.is_none(),
                "no unproven owner injected"
            );
            assert_eq!(unchanged.selected_candidate, restored.selected_candidate);
            assert_eq!(unchanged.planning_binding, restored.planning_binding);
        }
        let mut unavailable = restored.clone();
        assert!(!TranscodeManager::bind_prepared_candidate_owner(
            &mut unavailable,
            "prepared-owner",
            None,
        ));
        assert!(unavailable.owner_node_id.is_none());
        let mut already_owned = restored.clone();
        already_owned.owner_node_id = Some("another-owner".into());
        assert!(!TranscodeManager::bind_prepared_candidate_owner(
            &mut already_owned,
            "prepared-owner",
            Some(&eligible),
        ));
        assert_eq!(
            already_owned.owner_node_id.as_deref(),
            Some("another-owner")
        );
        // Old reader/manual byte shape remains exact when the field is absent.
        let mut legacy = job.supported_payload().expect("payload");
        if let JobPayload::CopyOutputPrepare {
            candidate_catalog, ..
        } = &mut legacy
        {
            *candidate_catalog = None;
        }
        let absent = serde_json::to_value(&legacy).expect("legacy serialization");
        assert!(absent.get("candidate_catalog").is_none());
        assert_eq!(
            serde_json::to_vec(&legacy).expect("legacy bytes"),
            serde_json::to_vec(
                &serde_json::from_value::<JobPayload>(absent.clone()).expect("old parser")
            )
            .expect("roundtrip")
        );
        let mut outer_unknown = absent.clone();
        outer_unknown["unknown_carrier_permission"] = true.into();
        assert!(serde_json::from_value::<JobPayload>(outer_unknown).is_err());
        let mut oversized = legacy.clone();
        if let JobPayload::CopyOutputPrepare {
            candidate_catalog, ..
        } = &mut oversized
        {
            *candidate_catalog = Some(serde_json::json!({"caps":"x".repeat(MAX_PAYLOAD_BYTES)}));
        }
        assert!(
            oversized.validate().is_err(),
            "entire payload still capped at 16KiB"
        );
        let encoded = JobPayload::EncodedOutputPrepare {
            encoded_output_version: 1,
            file_id: file.id,
            source_generation: "metadata:1".into(),
            source_size: file.size,
            source_mtime: file.mtime,
            source_object_version: "metadata:1".into(),
            policy_generation: "metadata:1".into(),
            intent: EncodedOutputIntent {
                target_node_id: "synthetic".into(),
                target_height: on.target_height,
                requested_height: Some(on.target_height),
                copy_for_burn: None,
                audio_index: queued.audio_index,
                audio_offset_ms: queued.audio_offset_ms,
                audio_claim: queued.audio_claim.clone().expect("actual claim"),
                audio_delivery: plurx_core::playback::audio::AudioDelivery {
                    action: plurx_core::playback::audio::AudioAction::None,
                    downmix: None,
                    reason: "metadata has no audio stream".into(),
                },
                subtitle_burn: None,
                subtitle_digest: None,
                hdr10_requested: false,
                grade: on.grade,
                normalized_geometry: on.normalized_geometry,
                profile: None,
                width: on.width,
                height: on.height,
                plan_digest: "a".repeat(64),
                executable_digest: "b".repeat(64),
                engine_digest: "c".repeat(64),
                candidate_id: Some(on.id),
                candidate_digest: Some(on.recipe_digest),
            },
            candidate_catalog: Some(carrier.clone()),
            scratch_bytes: 4096,
            reason: "recent_demand".into(),
        };
        encoded
            .validate()
            .expect("bounded encoded metadata payload");
        let roundtrip: JobPayload = serde_json::from_value(
            serde_json::to_value(&encoded).expect("encoded carrier serialization"),
        )
        .expect("strict payload parser");
        assert_eq!(
            roundtrip, encoded,
            "all closed encoded intent and carrier fields survive"
        );
        let mut manual_encoded = encoded.clone();
        if let JobPayload::EncodedOutputPrepare {
            candidate_catalog,
            intent,
            ..
        } = &mut manual_encoded
        {
            *candidate_catalog = None;
            intent.candidate_id = None;
            intent.candidate_digest = None;
            intent.normalized_geometry = false;
            intent.profile = None;
        }
        manual_encoded
            .validate()
            .expect("manual encoded absence remains supported");
        let manual_bytes = serde_json::to_vec(&manual_encoded).expect("manual bytes");
        assert!(!serde_json::to_value(&manual_encoded)
            .expect("manual shape")
            .as_object()
            .expect("payload object")
            .contains_key("candidate_catalog"));
        assert_eq!(
            manual_bytes,
            serde_json::to_vec(
                &serde_json::from_slice::<JobPayload>(&manual_bytes).expect("legacy manual parser")
            )
            .expect("exact manual bytes")
        );
        let mut unknown = carrier.clone();
        unknown["reorder_frames"] = true.into();
        assert!(
            serde_json::from_value::<crate::media_sessions::CandidateCatalogContext>(unknown)
                .is_err()
        );
        let mut oversized_caps = decoded.clone();
        oversized_caps.caps.video = vec![
            oversized_caps.caps.video[0].clone();
            plurx_core::playback::MAX_CLIENT_DECODER_ENTRIES + 1
        ];
        let mut refusals = vec![oversized_caps];
        let mut wrong_source = decoded.clone();
        wrong_source.binding.source_digest = "0".repeat(64);
        refusals.push(wrong_source);
        let mut stale_reorder = decoded.clone();
        stale_reorder.candidate = off.clone();
        refusals.push(stale_reorder);
        let mut wrong_route = decoded.clone();
        wrong_route.candidate.route = CandidateRoute::Remux;
        refusals.push(wrong_route);
        let mut wrong_raster = decoded.clone();
        wrong_raster.candidate.width += 2;
        refusals.push(wrong_raster);
        let mut wrong_grade = decoded.clone();
        wrong_grade.candidate.grade = OutputGrade::Hdr10;
        refusals.push(wrong_grade);
        let mut wrong_version = decoded.clone();
        wrong_version.caps.v = 1;
        refusals.push(wrong_version);
        for refusal in refusals {
            assert!(manager
                .restore_catalog_context(
                    &queued,
                    &refusal,
                    refusal.candidate.id,
                    file.size,
                    file.mtime,
                    None,
                    tokio::time::Instant::now() + Duration::from_secs(2)
                )
                .await
                .expect_err("bounded real restore refusal")
                .is_incompatible());
        }
        let mut wrong_offset = queued.clone();
        wrong_offset.audio_offset_ms = 15_001;
        let mut wrong_claim = queued.clone();
        wrong_claim.audio_claim = Some(plurx_core::playback::audio::AudioClaim {
            decoders: vec!["unknown".into()],
            sinks: vec![],
        });
        let mut wrong_delivery = queued.clone();
        wrong_delivery.audio_delivery = Some(plurx_core::playback::audio::AudioDelivery {
            action: plurx_core::playback::audio::AudioAction::None,
            downmix: None,
            reason: "x".repeat(257),
        });
        let mut wrong_request_route = queued.clone();
        wrong_request_route.kind = SessionKind::Copy {
            aac: false,
            preserve_dolby_vision: false,
            convert_dolby_vision: false,
        };
        for contradictory in [
            wrong_offset,
            wrong_claim,
            wrong_delivery,
            wrong_request_route,
        ] {
            assert!(manager
                .restore_catalog_context(
                    &contradictory,
                    &decoded,
                    on.id,
                    file.size,
                    file.mtime,
                    None,
                    tokio::time::Instant::now() + Duration::from_secs(2)
                )
                .await
                .expect_err("invalid audio cannot restore")
                .is_incompatible());
        }
        assert!(manager
            .restore_catalog_context(
                &queued,
                &decoded,
                on.id,
                file.size + 1,
                file.mtime,
                None,
                tokio::time::Instant::now() + Duration::from_secs(2)
            )
            .await
            .expect_err("source size mismatch")
            .is_incompatible());
        let mut target = queued.clone();
        target
            .candidate_context
            .as_mut()
            .expect("context")
            .owner_node_id = Some("other".into());
        assert!(manager
            .queued_candidate_catalog(&target, file, "synthetic")
            .await
            .is_err());
        let mut manual = queued.clone();
        manual.candidate_context = None;
        assert!(manager
            .queued_candidate_catalog(&manual, file, "synthetic")
            .await
            .expect("manual")
            .is_none());
        active.finish().await;
        for (id, size, mtime) in [
            (file.id + 99_999, file.size, file.mtime),
            (file.id, file.size + 1, file.mtime),
            (file.id, file.size, file.mtime + 1),
        ] {
            let (changed, active) = claimed_metadata(&store, file, None).await;
            assert!(TranscodeManager::claimed_preparation_file(
                store.as_ref(),
                id,
                size,
                mtime,
                &active.fence()
            )
            .await
            .expect("proved stale row")
            .is_none());
            let stopped = store
                .background_job(&changed.id)
                .await
                .expect("readback")
                .expect("row");
            assert_eq!(stopped.state, JobState::Cancelled);
            assert_eq!(stopped.failed_attempts, 0);
            assert_eq!(stopped.last_error_code.as_deref(), Some("source_changed"));
            assert!(!active
                .fence()
                .settle(JobSettlement::Yield {
                    error_code: None,
                    checkpoint: None,
                    not_before_ms: crate::media_sessions::unix_ms()
                })
                .await
                .expect("stopped token cannot yield again"));
            active.finish().await;
        }
        for (malformed, expected, target) in [
            (
                serde_json::json!({"unrecognized":true}),
                Some((on.id, on.recipe_digest)),
                "synthetic",
            ),
            (
                carrier.clone(),
                Some((off.id, off.recipe_digest)),
                "synthetic",
            ),
            (
                carrier.clone(),
                Some((on.id, on.recipe_digest)),
                "other-node",
            ),
        ] {
            let (refused, active) = claimed_metadata(&store, file, Some(malformed.clone())).await;
            let mut request = queued.clone();
            request.candidate_context = None;
            assert!(!manager
                .restore_queued_candidate(
                    &mut request,
                    file,
                    Some(&malformed),
                    expected,
                    target,
                    &active.fence(),
                    Instant::now() + Duration::from_secs(2)
                )
                .await
                .expect("typed/identity/target refusal"));
            assert_eq!(
                store
                    .background_job(&refused.id)
                    .await
                    .expect("readback")
                    .expect("row")
                    .state,
                JobState::Cancelled
            );
            assert!(
                request.candidate_context.is_none(),
                "no authority minted on refusal"
            );
            active.finish().await;
        }
        // A genuinely claimed legacy automatic row is cancelled through its
        // existing fence before any producer; no fake completion or reset.
        let (old, active) = claimed_metadata(&store, file, None).await;
        assert!(!manager
            .restore_queued_candidate(
                &mut manual,
                file,
                None,
                Some((on.id, on.recipe_digest)),
                "synthetic",
                &active.fence(),
                Instant::now() + Duration::from_secs(2)
            )
            .await
            .expect("legacy refusal"));
        let stopped = store
            .background_job(&old.id)
            .await
            .expect("readback")
            .expect("row");
        assert_eq!(stopped.state, JobState::Cancelled);
        assert_eq!(stopped.failed_attempts, 0);
        assert_eq!(
            stopped.last_error_code.as_deref(),
            Some("candidate_authority_stale_or_missing")
        );
        assert!(!active
            .fence()
            .settle(JobSettlement::Yield {
                error_code: None,
                checkpoint: None,
                not_before_ms: crate::media_sessions::unix_ms()
            })
            .await
            .expect("no token revival"));
        active.finish().await;
    }

    #[tokio::test]
    async fn tcl_fixture_catalog_uses_frozen_inputs_and_keeps_executable_rungs() {
        use plurx_core::{
            domain::{ItemKind, LibraryKind, NewItem, NewLibrary},
            store::SqliteStore,
        };
        let j: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../../docs/evidence/tcl-candidate-catalog-source-2026-10-02.json"
        ))
        .expect("retained source facts");
        let mut p = plurx_core::scan::probe::parse_probe_json(&j["probe"]);
        p.max_cll = Some(2259);
        p.max_fall = Some(183);
        p.mastering_max_luminance = Some(1000);
        let store: Arc<dyn Store> =
            Arc::new(SqliteStore::open_in_memory().expect("incident fixture operation"));
        let l = store
            .create_library(&NewLibrary {
                name: "incident".into(),
                kind: LibraryKind::Movies,
                paths: vec![],
                anime: false,
            })
            .await
            .expect("incident fixture operation");
        let item = store
            .insert_item(&NewItem {
                library_id: l.id,
                kind: ItemKind::Movie,
                parent_id: None,
                title: "fixture".into(),
                year: None,
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("incident fixture operation");
        let id = store
            .upsert_file(item, "/sanitized/movie.mkv", 18986891679, 1790349395, &p)
            .await
            .expect("incident fixture operation");
        let file = store
            .get_file(id)
            .await
            .expect("incident fixture operation")
            .expect("incident fixture operation");
        let base = crate::test_tempdir().expect("incident fixture operation");
        let manager = TranscodeManager::new(
            Arc::clone(&store),
            base.path().join("work"),
            EncoderCaps {
                vaapi: true,
                ..Default::default()
            },
            Pipeline::Cpu,
        )
        .with_cache(
            base.path().join("cache"),
            "incident-ffmpeg".into(),
            "incident".into(),
        )
        .with_decoders(vec!["hevc".into()]);
        let caps:plurx_core::playback::DeviceCaps=serde_json::from_value(serde_json::json!({"v":2,"video":[{"codec":"h264","decode":true,"present":["sdr"]}],"audio":["aac"],"transports":["hls"]})).expect("incident fixture operation");
        let planning = store
            .playback_planning_snapshot(id, &QUALITY_PLANNING_KEYS)
            .await
            .expect("incident fixture operation")
            .expect("incident fixture operation");
        assert_eq!(planning.file.id, file.id);
        let rows = manager
            .quality_candidates_from_snapshot_progress(
                &planning,
                &caps,
                Some(1),
                0,
                None,
                Presentation::Vod,
                None,
                None,
                None,
            )
            .await;
        let mut context =
            TranscodeManager::candidate_context(rows.first().expect("healthy catalog control"));
        context.canonical_caps = Some(caps.clone());
        context.planning_binding =
            Some(crate::media_pool::PlanningBinding::from_snapshot(&planning));
        let mut request = SessionRequest {
            sdr_master_codecs: None,
            continuous_media: None,
            quality_catalog: None,
            candidate_context: Some(Box::new(context)),
            vod_only: false,
            passive_vod: false,
            finite_bitrate_limit_bps: None,
            file_id: id,
            playback_id: "binding-regression".to_owned(),
            audio_claim: None,
            audio_delivery: None,
            request_id: None,
            control_sequence: None,
            automatic: true,
            previous_session_id: None,
            reopen_reason: None,
            kind: SessionKind::Transcode { height: 720 },
            start_seconds: 0.0,
            audio_index: Some(1),
            audio_offset_ms: 0,
            subtitle_burn: None,
            hdr10: false,
            presentation: Presentation::Vod,
            block_budget_secs: None,
            transport: None,
        };
        manager
            .validate_candidate_planning_binding(&request, None)
            .await
            .expect("unchanged snapshot is admissible");
        // A later settings revision must not leak into this enumeration.
        store
            .put_setting(keys::HWACCEL, "software")
            .await
            .expect("incident fixture operation");
        store
            .put_setting(keys::TRANSCODE_RATE_MODE, "quality")
            .await
            .expect("incident fixture operation");
        for presentation in [Presentation::Vod, Presentation::Live] {
            for kind in [
                SessionKind::Transcode { height: 720 },
                SessionKind::Copy {
                    aac: true,
                    preserve_dolby_vision: false,
                    convert_dolby_vision: false,
                },
            ] {
                request.presentation = presentation;
                request.kind = kind;
                let error = manager
                    .create_session(&request, "binding-regression")
                    .await
                    .err()
                    .expect("changed generation must fail before either producer starts");
                assert!(crate::transcode::is_catalog_input_error(&error), "{error}");
            }
        }
        let repeated = manager
            .quality_candidates_from_snapshot_progress(
                &planning,
                &caps,
                Some(1),
                0,
                None,
                Presentation::Vod,
                None,
                None,
                None,
            )
            .await;
        assert_eq!(
            rows.iter().map(|row| row.id).collect::<Vec<_>>(),
            repeated.iter().map(|row| row.id).collect::<Vec<_>>()
        );
        let heights: Vec<_> = rows.iter().map(|row| row.target_height).collect();
        for height in [144, 240, 360, 480, 720, 1080, 1918] {
            assert!(
                heights.contains(&height),
                "missing incident rung {height}: {heights:?}"
            );
        }
        let different_audio = manager
            .quality_candidates_from_snapshot_progress(
                &planning,
                &caps,
                Some(0),
                0,
                None,
                Presentation::Vod,
                None,
                None,
                None,
            )
            .await;
        assert!(!different_audio.is_empty());
        assert!(
            rows.iter()
                .all(|row| different_audio.iter().all(|other| row.id != other.id)),
            "an audio change must replace recipe identities"
        );
        for selected in &rows {
            let followup = manager
                .quality_candidates_from_snapshot_progress(
                    &planning,
                    &caps,
                    Some(1),
                    0,
                    None,
                    Presentation::Vod,
                    None,
                    None,
                    Some(selected),
                )
                .await;
            assert!(
                followup
                    .iter()
                    .any(|row| row.id == selected.id && row.decoder_compatible),
                "selected recipe must survive a follow-up without a synthetic capability document"
            );
        }
        assert!(rows.iter().all(|row| row.decoder_compatible));
    }
}
