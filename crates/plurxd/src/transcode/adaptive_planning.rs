//! Pure recipe refinement while the admitted P collector remains retained.
//! Source/executable acquisition stays inside core; the same M lease follows
//! all resulting Encoding aliases through dormant prepare and serving.
use super::*;
use plurx_core::adaptive_resources::runtime::planning_probe::FixedProbeMetadataLease;

impl TranscodeManager {
    /// Resolve the supported finite software route using fresh admitted facts.
    /// The worker keeps the original P owner active through this method and
    /// the prepared recipe capacity check. This method performs no source
    /// open, engine capture, or native probe.
    pub(crate) async fn resolve_adaptive_encoding_from_probe(
        &self,
        request: &SessionRequest,
        file: &plurx_core::domain::MediaFile,
        planning: &plurx_core::store::PlaybackPlanningSnapshot,
        report: FixedProbeMetadataLease,
    ) -> Result<Arc<crate::vodencode::Encoding>, String> {
        if request.file_id != file.id
            || planning.file.id != file.id
            || planning.file.size != file.size
            || planning.file.mtime != file.mtime
            || report.source_file_id() != file.id.to_string()
            || report.source_path() != file.path
            || report.scanner_size() != file.size
            || report.scanner_mtime() != file.mtime
        {
            return Err(vod_refusal_error(
                "vod_source_rescan_required",
                "the admitted planning source differs from the selected catalog row",
            ));
        }
        if request.subtitle_burn.is_some()
            || request.continuous_media.is_some()
            || request.dv_processing_destination.is_some()
            || !file.dolby_vision.is_empty()
        {
            return Err(vod_refusal_error(
                "vod_decoder_plan_refused",
                "this route requires a different owned planning profile",
            ));
        }
        let source_height = file.height.filter(|height| *height >= 2).ok_or_else(|| {
            vod_refusal_error(
                "vod_video_geometry_unknown",
                "the source has no usable video height",
            )
        })?;
        let target_height = match request.kind {
            SessionKind::Transcode { height } if height > 0 => {
                if request.candidate_context.is_some() {
                    height
                } else {
                    height.min(source_height)
                }
            }
            _ => {
                return Err(vod_refusal_error(
                    "vod_invalid_height",
                    "adaptive finite planning requires an encoded video request",
                ))
            }
        }
        .max(2)
            & !1;
        if transcode::output_size(file, target_height).is_none() {
            return Err(vod_refusal_error(
                "vod_video_geometry_unknown",
                "the source has no usable video dimensions",
            ));
        }
        if request
            .audio_index
            .is_some_and(|index| index < 0 || index as usize >= file.audio_streams.len())
        {
            return Err(vod_refusal_error(
                "vod_audio_track_missing",
                "the selected audio track is missing",
            ));
        }
        let stored = planning.probe_json.as_deref().ok_or_else(|| {
            vod_refusal_error(
                "vod_source_rescan_required",
                "this file has no stored probe; reanalyze it before playback",
            )
        })?;
        let comparison = crate::ffmpeg::compare_probe_documents(stored, report.document())
            .map_err(|error| vod_refusal_error("vod_source_rescan_required", error))?;
        if !comparison.same {
            return Err(vod_refusal_error("vod_source_rescan_required", format!(
                "the stored probe differs from the source at {}; reanalyze this item before playback",
                comparison.rendered_differences(),
            )));
        }
        if comparison.admitted_on_reporter_drift {
            crate::ffmpeg::note_reporter_drift_admission();
        }
        let source_clock =
            crate::transcode::source_preparation::held_source_clock(report.document())
                .map_err(|error| vod_refusal_error("vod_source_rescan_required", error))?;
        let mut grid = crate::vodencode::frame_grid(Some(stored)).ok_or_else(|| {
            vod_refusal_error(
                "vod_frame_cadence_unknown",
                "the source probe has no usable video cadence; rescan the file",
            )
        })?;
        let encoder = Encoder::Software;
        let software_threads = crate::vodencode::frozen_software_threads(
            &Workload::of(file, target_height),
            self.software_budget().await,
        );
        let options = self.live_lookup_options(
            self.rate_control_snapshot(),
            encoder,
            file,
            target_height,
            0.0,
            request.audio_index,
            None,
            Some(software_threads),
            OutputGrade::Sdr,
        );
        let mut options = self.encoded_start_audio_options(request, file, options)?;
        if let Some(context) = request.candidate_context.as_ref() {
            options.normalized_geometry = context.normalized_geometry;
            if let Some(profile) = context.profile {
                options.auto_quality_rate_profile = Some(profile);
                options.video_bitrate_kbps = profile.video_bitrate_kbps();
                options.effective_rate_control = EffectiveRateControl::Vbr;
            }
        }
        options.output_codec =
            Self::selected_output_codec(request.candidate_context.as_deref(), encoder);
        manager_create::constrain_finite_vod_rate(request, file, &mut options)?;
        let plan = BoundPlanCaller::Vod.finish(self.resolve_movie_plan_with_processing_context(
            file,
            &options,
            encoder,
            report.facts(),
            &AttemptRestrictions::none(),
            None,
        ))?;
        options.pipeline = plan.options().pipeline;
        options.strict_dolby = plan.options().strict_dolby.clone();
        if plan.decode().backend() != transcode::DecodeBackend::Software
            || plan.output_contract().output_codec() != "h264"
            || options.strict_dolby.is_some()
            || plan.macos_processing_identity().is_some()
        {
            return Err(vod_refusal_error(
                "vod_decoder_plan_refused",
                "the locally resolved plan requires a different owned execution profile",
            ));
        }
        if let Some(rate) = plan
            .output_contract()
            .normalized_geometry()
            .and_then(|geometry| geometry.rate_profile.and(geometry.frame_rate))
        {
            grid = transcode::VodFrameGrid::new(rate.numerator(), rate.denominator()).ok_or_else(
                || {
                    vod_refusal_error(
                        "vod_frame_cadence_unknown",
                        "the normalized output cadence is invalid",
                    )
                },
            )?;
        }
        let output = plan.output_contract();
        let same_raster = transcode::output_size(file, options.target_height)
            == output
                .effective_width()
                .zip(output.effective_height())
                .map(|(width, height)| (i64::from(width), i64::from(height)));
        let cadence = same_raster
            .then(|| transcode::Rational::new(grid.numerator, grid.denominator))
            .flatten();
        let plan = plan
            .with_sdr_avc_qualification(&self.caps, cadence, options.force_idr)
            .with_vaapi_hdr_policy(&self.caps);
        let reorder_frames = Self::vod_reorder_from_snapshot(planning);
        if let Some(context) = request.candidate_context.as_ref() {
            self.validate_prepared_candidate_recipe(
                &plan,
                request.presentation,
                reorder_frames,
                context,
            )?;
        }
        let (executable, engine, ffmpeg_build) =
            crate::ffmpeg::EncodedEngine::from_fixed_planning_attestation(report.engine())?;
        let source_object_version = report.source_object_version().to_owned();
        let resources = TranscodeResourceEstimate::of(&plan, &Workload::of(file, target_height));
        let encoding = Arc::new(crate::vodencode::Encoding {
            source_object_version,
            source_clock,
            plan,
            resources,
            options,
            grid,
            dv_processing: Self::vod_dv_processing_from_snapshot(planning, request)?,
            dv_runtime: None,
            shared_audio: None,
            reorder_frames,
            subtitle: None,
            subtitle_digest: None,
            ffmpeg_build,
            executable,
            engine,
            admissions: self.admissions.clone(),
            store: Arc::clone(&self.store),
            nonpreemptive_trial: request.automatic && request.candidate_context.is_some(),
            candidate_recipe: request
                .candidate_context
                .as_ref()
                .map(|context| context.recipe_digest),
            production_proofs: Arc::clone(&self.candidate_production_proofs),
            active_production: std::sync::Mutex::new(
                crate::vodencode::ActiveProductionWindow::default(),
            ),
            speculative: AtomicBool::new(false),
            queued: std::sync::Mutex::new(None),
            policy_retry: AtomicBool::new(false),
            handoff_wait: AtomicBool::new(false),
            last_refusal: std::sync::Mutex::new(None),
            handoff_claim: std::sync::Mutex::new(None),
            hooks: Box::new(crate::vodencode::NoopEncodingHooks),
            planning_metadata: Some(report),
        });
        Ok(encoding)
    }
}
