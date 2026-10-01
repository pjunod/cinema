use super::*;
use plurx_core::playback::candidate::{CandidateId, CandidateRoute, QualityCandidate};

impl TranscodeManager {
    /// Resolve the same output contracts used at dispatch. This never reserves
    /// capacity: incomplete cache verification and unknown production remain
    /// unknown, and a later owner must resolve and compare the full recipe.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn quality_candidates_with_copy_contract(
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
    ) -> Vec<QualityCandidate> {
        let mut catalog_file = file.clone();
        catalog_file.audio_offset_ms = audio_offset_ms;
        let file = &catalog_file;
        let audio = self.candidate_audio_index(file, audio).await;
        let Ok(audio_claim) = plurx_core::playback::audio::AudioClaim::from_caps(caps) else {
            return Vec::new();
        };
        let audio_claim = retained_claim.cloned().or(audio_claim);
        let source_facts = self.quality_source_facts(file).await;
        let coded_rate = source_facts
            .as_ref()
            .and_then(|facts| facts.frame_rate().value())
            .map(|rate| (rate.numerator(), rate.denominator()));
        let mut result = Vec::new();
        let mut profile = plurx_core::playback::DeviceProfile::from_caps_v2(caps);
        profile.retain_applicable_learned_limits(crate::media_sessions::unix_ms());
        let mut node =
            plurx_core::playback::RenderCaps::strip_only(crate::ffmpeg::has_dovi_rpu().await);
        node.dolby_vision_convert = self.dv_convert_enabled().await;
        let copy_decision = plurx_core::playback::decide(file, &profile, &node);
        let (copy_audio, copy_dv, copy_conversion) = retained_copy.unwrap_or((
            copy_decision.transcode_audio,
            copy_decision.preserve_dolby_vision,
            copy_decision.convert_dolby_vision,
        ));
        let copy_engine = crate::ffmpeg::EncodedExecutable::capture().await.ok();
        let copy_runtime_engine = crate::ffmpeg::EncodedEngine::capture(None).await.ok();
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
                result.push(QualityCandidate {
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
                });
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
            if !normalized_geometry && !legacy_geometry_known {
                continue;
            }
            let Ok((encoder, grade)) = self
                .encoder_and_grade_for(file, hdr_requested, height, subtitle.is_some())
                .await
            else {
                continue;
            };
            if hdr_requested && grade != OutputGrade::Hdr10 {
                continue;
            }
            let mut options = self.live_lookup_options(
                self.rate_control_snapshot(),
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
            let Ok(plan) = self.resolve_movie_plan(file, &options, encoder).await else {
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
            result.push(QualityCandidate {
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
            });
        }
        result
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
        let index = crate::decode_facts::absolute_video_ordinal(&probe, 0)?;
        let catalog = DecodeCatalogMetadata::from_media_file(file).ok()?;
        crate::decode_facts::legacy_ordinal_facts(
            &probe,
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
            return Ok(());
        };
        let snapshot = envelope
            .decoder_caps
            .as_ref()
            .ok_or_else(|| "candidate decoder snapshot missing".to_owned())?;
        let file = self
            .store
            .get_file(envelope.request.file_id)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "candidate source missing".to_owned())?;
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
        let candidates = self
            .quality_candidates_with_copy_contract(
                &file,
                &snapshot.device_caps(),
                envelope.request.audio_index,
                envelope.request.audio_offset_ms,
                envelope.request.subtitle_burn,
                envelope.request.presentation,
                retained_copy,
                envelope.request.audio_delivery.as_ref(),
                envelope.request.audio_claim.as_ref(),
            )
            .await;
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
        envelope.request.candidate_context = Some(Self::candidate_context(candidate));
        Ok(())
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

    pub(crate) fn candidate_context(candidate: &QualityCandidate) -> CandidateExecutionContext {
        CandidateExecutionContext {
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
