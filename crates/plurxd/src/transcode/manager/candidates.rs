use super::*;
use plurx_core::playback::candidate::{CandidateId, CandidateRoute, QualityCandidate};

impl TranscodeManager {
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
        let mut catalog_file = snapshot.file.clone();
        catalog_file.audio_offset_ms = audio_offset_ms;
        let file = &catalog_file;
        let audio = Self::candidate_audio_from_snapshot(snapshot, audio);
        let probe: serde_json::Value = match snapshot.probe_json.as_deref() {
            Some(encoded) => match serde_json::from_str(encoded) {
                Ok(probe) => probe,
                Err(_) => return Vec::new(),
            },
            None => Self::catalog_plan_probe(file),
        };
        let source_facts = Self::quality_facts_from_probe(file, &probe);
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
        let copy_engine = if copy_possible {
            crate::ffmpeg::EncodedExecutable::capture().await.ok()
        } else {
            None
        };
        let copy_runtime_engine = if copy_possible {
            crate::ffmpeg::EncodedEngine::capture(None).await.ok()
        } else {
            None
        };
        let copy_source = crate::fragment_index_cluster::open_source_fence(file, None)
            .await
            .ok();
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
                        copy_audio,
                        copy_dv,
                        copy_conversion,
                        file.dolby_vision,
                        copy_source.as_ref().map(|source| source.object_version()),
                        copy_engine.as_ref().map(|engine| engine.digest.as_str()),
                        copy_runtime_engine
                            .as_ref()
                            .map(|engine| engine.digest.as_str()),
                        width,
                        height,
                        transcode::routing_hdr(file),
                    ]))
                    .expect("bounded candidate source identity is serializable"),
                );
                let recipe_digest: [u8; 32] = hash.finalize().into();
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
                    grade: if transfer == plurx_core::playback::Transfer::Sdr {
                        OutputGrade::Sdr
                    } else {
                        OutputGrade::Hdr10
                    },
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
            let Ok((encoder, grade)) = self
                .encoder_and_grade_for_with_preference(
                    file,
                    hdr_requested,
                    height,
                    subtitle.is_some(),
                    preference,
                )
                .await
            else {
                continue;
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
            let Ok(recipe_digest) = self.candidate_recipe_digest(&plan, presentation) else {
                continue;
            };
            let complete_cache = match presentation {
                Presentation::Live => self.verified_cache_hit(&plan).await,
                Presentation::Vod => {
                    self.candidate_complete_vod_cache(
                        file.id,
                        &plan,
                        copy_source.as_ref().map(|source| source.object_version()),
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
        result
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

    pub(crate) async fn restore_candidate_context(
        &self,
        envelope: &mut crate::media_sessions::RemoteStartRequest,
    ) -> Result<(), String> {
        let Some(id) = envelope.candidate_id else {
            if envelope
                .request
                .continuous_media
                .as_ref()
                .is_some_and(|media| media.autonomous_companion.is_some())
            {
                return Err("autonomous family primary catalog identity missing".to_owned());
            }
            return Ok(());
        };
        let catalog = envelope
            .candidate_catalog
            .as_ref()
            .ok_or_else(|| "candidate canonical evidence missing".to_owned())?;
        let planning = self
            .store
            .playback_planning_snapshot(envelope.request.file_id, &QUALITY_PLANNING_KEYS)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "candidate source missing".to_owned())?;
        if catalog.binding != crate::media_pool::PlanningBinding::from_snapshot(&planning) {
            return Err("candidate source or settings changed".to_owned());
        }
        let file = &planning.file;
        if file.size != envelope.source_size || file.mtime != envelope.source_mtime {
            return Err("candidate source changed".to_owned());
        }
        let retained_copy = match envelope.request.kind {
            SessionKind::Copy {
                aac,
                preserve_dolby_vision,
                convert_dolby_vision,
            } => Some((aac, preserve_dolby_vision, convert_dolby_vision)),
            SessionKind::Transcode { .. } => None,
        };
        let mut canonical_caps = catalog.caps.clone();
        if let Some(snapshot) = envelope.decoder_caps.as_ref() {
            canonical_caps.video = snapshot.device_caps().video;
        }
        let selected = &catalog.candidate;
        if selected.id != id {
            return Err("candidate descriptor identity mismatch".to_owned());
        }
        let mut candidates = self
            .quality_candidates_from_snapshot_progress(
                &planning,
                &canonical_caps,
                envelope.request.audio_index,
                envelope.request.audio_offset_ms,
                envelope.request.subtitle_burn,
                envelope.request.presentation,
                retained_copy,
                None,
                Some(selected),
            )
            .await;
        if let Some(media) = envelope.request.continuous_media.as_ref() {
            if let Some(companion_id) = media.autonomous_companion {
                let selected_companion = media
                    .companion_catalog
                    .as_ref()
                    .filter(|row| row.id == companion_id && row.identity_matches())
                    .ok_or_else(|| "continuous companion canonical evidence missing".to_owned())?;
                candidates.extend(
                    self.quality_candidates_from_snapshot_progress(
                        &planning,
                        &canonical_caps,
                        envelope.request.audio_index,
                        envelope.request.audio_offset_ms,
                        envelope.request.subtitle_burn,
                        envelope.request.presentation,
                        retained_copy,
                        None,
                        Some(selected_companion),
                    )
                    .await,
                );
            }
        }
        let candidate = candidates
            .iter()
            .find(|candidate| candidate.id == id && candidate.decoder_compatible)
            .ok_or_else(|| "candidate recipe or worker changed".to_owned())?;
        match envelope.request.kind {
            SessionKind::Transcode { height }
                if candidate.route == CandidateRoute::Encode
                    && height == i64::from(candidate.target_height)
                    && envelope.request.hdr10 == (candidate.grade == OutputGrade::Hdr10) => {}
            SessionKind::Copy { .. } if candidate.route != CandidateRoute::Encode => {}
            _ => return Err("candidate delivery mismatch".to_owned()),
        }
        if let Some(media) = envelope.request.continuous_media.as_mut() {
            media.companion_context = match media.autonomous_companion {
                Some(companion_id) => {
                    let companion = candidates
                        .iter()
                        .find(|row| {
                            row.id == companion_id
                                && row.id != candidate.id
                                && row.decoder_compatible
                                && row.route == CandidateRoute::Encode
                                && row.normalized_geometry
                                && row.grade == OutputGrade::Sdr
                                && row.target_height != candidate.target_height
                        })
                        .ok_or_else(|| {
                            "autonomous companion recipe or decoder changed".to_owned()
                        })?;
                    Some(Box::new(ContinuousCompanionContext {
                        height: i64::from(companion.target_height),
                        candidate: {
                            let mut context = Self::candidate_context(companion);
                            context.canonical_caps = Some(canonical_caps.clone());
                            context.planning_binding =
                                Some(crate::media_pool::PlanningBinding::from_snapshot(&planning));
                            context
                        },
                    }))
                }
                None => None,
            };
        }
        let mut context = Self::candidate_context(candidate);
        context.canonical_caps = Some(canonical_caps);
        context.planning_binding =
            Some(crate::media_pool::PlanningBinding::from_snapshot(&planning));
        envelope.request.candidate_context = Some(Box::new(context));
        Ok(())
    }

    pub(crate) fn candidate_context(candidate: &QualityCandidate) -> CandidateExecutionContext {
        CandidateExecutionContext {
            canonical_caps: None,
            selected_candidate: candidate.clone(),
            planning_binding: None,
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

pub(crate) const QUALITY_PLANNING_KEYS: [&str; 9] = [
    keys::HWACCEL,
    keys::DV_CONVERT,
    keys::AUDIO_LANG,
    keys::SUB_LANG,
    keys::SUB_MODE,
    keys::PLAYBACK_DISPLAY_AWARE_AUTO,
    keys::PLAYBACK_CONTROL_PROTOCOL_V1,
    keys::TRANSCODE_RATE_MODE,
    keys::TRANSCODE_QUALITY,
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
            continuous_media: None,
            quality_catalog: None,
            candidate_context: Some(Box::new(context)),
            file_id: id,
            playback_id: "binding-regression".to_owned(),
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
