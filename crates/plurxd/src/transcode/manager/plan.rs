use super::*;

impl TranscodeManager {
    /// This node's output identity, for naming a transcode.
    pub(super) fn digest(&self) -> Option<PipelineDigest> {
        let cache = self.cache.as_ref()?;
        Some(PipelineDigest {
            ffmpeg_build: cache.ffmpeg_build.clone(),
        })
    }

    /// The stored-probe path has no descriptor to fingerprint, so it derives a
    /// stable placeholder from the catalog row. This is *not* evidence that the
    /// file still holds the bytes that were probed; plans built this way carry
    /// [`PlanSourceBinding::CatalogRow`] and callers that need proof must
    /// refuse them. It no longer enters any artifact key — the key uses
    /// [`DecodeCacheIdentity`], which every path computes identically.
    fn plan_source_identity(
        file: &plurx_core::domain::MediaFile,
    ) -> Result<DecodeSourceIdentity, String> {
        let mut digest = Sha256::new();
        for value in [
            file.id.to_string(),
            file.size.to_string(),
            file.mtime.to_string(),
        ] {
            digest.update((value.len() as u64).to_be_bytes());
            digest.update(value.as_bytes());
        }
        DecodeSourceIdentity::from_sha256(hex::encode(digest.finalize()))
            .map_err(|error| error.to_string())
    }

    /// What the catalog row alone can say about the source, as FFprobe would
    /// have said it.
    ///
    /// `probe_json IS NULL` is an expected, first-class row state — the scan's
    /// repair pass and the probe retry job exist for exactly those rows — so a
    /// missing probe cannot be allowed to mean "this title is unplayable".
    /// Before decode planning, such a file transcoded fine: the command named
    /// no decoder and FFmpeg read the container. Planning has to be able to say
    /// the same thing.
    ///
    /// Everything here is a value the scan actually recorded. There is no frame
    /// rate, because the row has none and inventing 24 fps would be exactly the
    /// silent default the plan forbids; the facts carry it as unknown. The
    /// resulting plan differs from a stored-probe plan and therefore names a
    /// different artifact, which is correct — they are different measurements,
    /// and the one made from real probe output wins the moment it exists.
    fn catalog_plan_probe(file: &plurx_core::domain::MediaFile) -> serde_json::Value {
        let transfer = match transcode::routing_hdr(file) {
            Some("hdr10" | "hdr10plus" | "dolby_vision") => Some("smpte2084"),
            Some("hlg") => Some("arib-std-b67"),
            _ => Some("bt709"),
        };
        serde_json::json!({
            "streams": [{
                "index": 0,
                "codec_type": "video",
                "codec_name": file.video_codec,
                "profile": file.video_profile,
                "width": file.width,
                "height": file.height,
                "pix_fmt": if file.bit_depth.unwrap_or(8) >= 10 { "yuv420p10le" } else { "yuv420p" },
                "color_transfer": transfer,
                "disposition": {"attached_pic": 0}
            }]
        })
    }

    pub(super) async fn resolve_movie_plan(
        &self,
        file: &plurx_core::domain::MediaFile,
        options: &TranscodeOptions,
        encoder: Encoder,
    ) -> Result<ResolvedTranscode, String> {
        self.resolve_restricted_movie_plan(file, options, encoder, &AttemptRestrictions::none())
            .await
    }

    /// The same resolution, under a decode restriction.
    ///
    /// Separate from [`Self::resolve_movie_plan`] rather than a parameter on
    /// it, because every existing caller wants the unrestricted answer and a
    /// fourth positional argument spelled `&AttemptRestrictions::none()` at
    /// two dozen call sites is a place for the wrong value to be pasted.
    ///
    /// A restricted plan is a **different artifact**: the decode backend feeds
    /// `plan_digest`, so a generation produced under a restriction has its own
    /// cache identity and the cache cannot serve one for the other. That is
    /// intended.
    ///
    /// It also changes `TranscodeResourceEstimate::of`, because forcing
    /// software decode rewrites what the pipeline costs. The session is
    /// already paying for what it reserved at admission, so what a mid-session
    /// alternate owes is the *difference* — `build` records the restricted
    /// plan's whole estimate as `cpu_total` and the executor takes
    /// `cpu_total - software_threads_held()`. Asking for the whole estimate
    /// again would make the session pay twice to end up owning once, and on a
    /// box where twice does not fit the recovery would fail over capacity it
    /// already held.
    ///
    /// And it can rewrite the renderer, because a vendor-native graph accepts
    /// only its own decoder. That is why
    /// [`PrepublicationTranscodeRetry::prepare_decode_restricted`] reads the
    /// pipeline off the plan this returns rather than off the options it was
    /// given.
    pub(super) async fn resolve_restricted_movie_plan(
        &self,
        file: &plurx_core::domain::MediaFile,
        options: &TranscodeOptions,
        encoder: Encoder,
        restrictions: &AttemptRestrictions,
    ) -> Result<ResolvedTranscode, String> {
        let stored_probe = self
            .store
            .get_file_probe_json(file.id)
            .await
            .map_err(|error| format!("reading decoder planning facts: {error}"))?;
        // One expression for both builds: a row without probe output plans
        // from what the scan recorded rather than refusing to play.
        let probe: serde_json::Value = match stored_probe.as_deref() {
            Some(encoded) => serde_json::from_str(encoded)
                .map_err(|error| format!("decoder planning facts are invalid: {error}"))?,
            None => Self::catalog_plan_probe(file),
        };
        let source_identity = Self::plan_source_identity(file)?;
        let catalog = DecodeCatalogMetadata::from_media_file(file)
            .map_err(|error| format!("decoder catalog facts are invalid: {error}"))?;
        // The same stream the bound route selects, by the same rule. This used
        // to ask for "the first playable video stream" while the bound route
        // asked for FFmpeg's `0:v:0`. On a file carrying cover art those are
        // two different streams, so the producer and the player planned
        // different work and named different artifacts — and only one of them
        // matched the command the shipping builder emits. One rule, and it is
        // the one the command uses.
        let index = crate::decode_facts::absolute_video_ordinal(&probe, 0)
            .ok_or_else(|| "decoder planning facts name no video stream".to_owned())?;
        let facts = crate::decode_facts::legacy_ordinal_facts(
            &probe,
            source_identity,
            index,
            Some(&catalog),
        )
        .map_err(|error| format!("decoder planning facts are incompatible: {error}"))?;
        self.resolve_movie_plan_with_facts(file, options, encoder, &facts, restrictions)
    }

    fn resolve_movie_plan_with_facts(
        &self,
        file: &plurx_core::domain::MediaFile,
        options: &TranscodeOptions,
        encoder: Encoder,
        facts: &DecodeFacts,
        restrictions: &AttemptRestrictions,
    ) -> Result<ResolvedTranscode, String> {
        use plurx_core::transcode::ArtifactQualification;

        #[cfg(test)]
        if self
            .force_artifact_qualification
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return self.resolve_movie_plan_with_qualification(
                file,
                options,
                encoder,
                facts,
                restrictions,
                self.artifact_qualification(),
            );
        }

        // Resolve once under the stable identity every node already has. The
        // decode backend does not depend on artifact qualification, and this
        // gives us the exact path whose contract must be checked without
        // guessing from the encoder or the host.
        let unqualified = self.resolve_movie_plan_with_qualification(
            file,
            options,
            encoder,
            facts,
            restrictions,
            ArtifactQualification::Unqualified,
        )?;
        if !self.published_artifact_qualification().requested {
            return Ok(unqualified);
        }
        let Some(codec) = unqualified.decode().input_codec() else {
            return Ok(unqualified);
        };
        let backend = unqualified.decode().backend();
        let Some(decoder) = self.measured_decoders.implementation(codec, backend) else {
            return Ok(unqualified);
        };
        if self
            .diagnostic_policy
            .contract_for(
                codec,
                decoder,
                backend.name(),
                crate::decoder_health::QUALIFIED_STDERR_MODE,
            )
            .is_none()
        {
            return Ok(unqualified);
        }

        let qualified = self.resolve_movie_plan_with_qualification(
            file,
            options,
            encoder,
            facts,
            restrictions,
            ArtifactQualification::HealthQualified,
        )?;
        if qualified.decode().backend() != backend
            || qualified.decode().input_codec() != Some(codec)
        {
            tracing::warn!(
                codec,
                backend = backend.name(),
                "artifact qualification changed the resolved decode path; keeping its stable identity"
            );
            return Ok(unqualified);
        }
        Ok(qualified)
    }

    fn resolve_movie_plan_with_qualification(
        &self,
        file: &plurx_core::domain::MediaFile,
        options: &TranscodeOptions,
        encoder: Encoder,
        facts: &DecodeFacts,
        restrictions: &AttemptRestrictions,
        qualification: plurx_core::transcode::ArtifactQualification,
    ) -> Result<ResolvedTranscode, String> {
        let build = self
            .cache
            .as_ref()
            .map_or("unconfigured-ffmpeg", |cache| cache.ffmpeg_build.as_str());
        let identity = DecodeCapabilitySnapshotIdentity::new(
            hex::encode(Sha256::digest(build.as_bytes())),
            "legacy-unqualified-node".to_owned(),
            None,
        )
        .map_err(|error| error.to_string())?;
        let qualifying = qualification.enforces_receipt();
        #[allow(unused_mut)]
        let mut decoders = self.decoders.clone();
        #[cfg(test)]
        if decoders.is_empty() {
            if let Some(codec) = facts.codec() {
                decoders.push(codec.to_owned());
            }
        }
        let capabilities = DecodeCapabilities::new(
            identity,
            Vec::new(),
            decoders
                .into_iter()
                // A measured name, or none — never the family. This node's
                // advertised inventory is family names, and a family is not an
                // implementation: naming `av1` here would force `-c:v av1`
                // where FFmpeg would otherwise choose `libdav1d`, and bake
                // that different, slower decoder into the artifact's identity.
                //
                // The measurement is read only under the qualified identity.
                // Naming a decoder changes an artifact's name, so letting a
                // boot probe do it for every node would rotate an unqualified
                // fleet's whole cache in exchange for a value nothing there
                // enforces. Under the qualified identity it is the point: a
                // diagnostic contract is qualified against a *named* decoder,
                // so a plan that names none can never be matched to one, and
                // an attempt nothing can classify can never be certified.
                .map(|codec| SoftwareDecoder {
                    implementation: qualifying
                        .then(|| {
                            self.measured_decoders.implementation(
                                &codec,
                                plurx_core::transcode::DecodeBackend::Software,
                            )
                        })
                        .flatten()
                        .map(str::to_owned),
                    codec,
                })
                .collect(),
        )
        .map_err(|error| format!("decoder capability snapshot is invalid: {error}"))?;
        let compatibility = std::env::var("PLURX_HWDECODE").ok();
        let policy = DecodePolicySnapshot::new(DecodePlanPolicy::Legacy, compatibility.as_deref())
            .qualifying_artifacts(qualification);
        transcode::resolve_transcode(
            &TranscodeRequest::new(
                encoder,
                TranscodeMediaOptions::from_options_with_facts(file, options, facts),
            ),
            facts,
            &capabilities,
            &policy,
            restrictions,
        )
        .map_err(|error| format!("decoder plan refused: {error}"))
    }

    pub(super) async fn resolve_bound_movie_plan(
        &self,
        file: &plurx_core::domain::MediaFile,
        options: &TranscodeOptions,
        encoder: Encoder,
        source: &Arc<BoundPretranscodeSource>,
        deadline: Instant,
        cancelled: Option<&tokio_util::sync::CancellationToken>,
    ) -> Result<ResolvedTranscode, String> {
        BoundPlanCaller::Pretranscode.finish(
            self.resolve_held_movie_plan(
                file,
                options,
                encoder,
                crate::decode_facts::DecodeFactSource::new(
                    Arc::clone(&source.handle),
                    Arc::clone(&source.offset_gate),
                ),
                deadline,
                cancelled,
            )
            .await,
        )
    }

    /// Resolve decoder facts through the exact source description the caller
    /// already owns. Both background artifacts and immutable VOD use this
    /// seam; neither is allowed to derive its key from scanner-time facts and
    /// later run a different object opened by name.
    pub(super) async fn resolve_held_movie_plan(
        &self,
        file: &plurx_core::domain::MediaFile,
        options: &TranscodeOptions,
        encoder: Encoder,
        fact_source: crate::decode_facts::DecodeFactSource,
        deadline: Instant,
        cancelled: Option<&tokio_util::sync::CancellationToken>,
    ) -> Result<ResolvedTranscode, String> {
        let Some(probe) = self.decode_probe_identity.as_ref() else {
            return self.resolve_movie_plan(file, options, encoder).await;
        };
        let catalog = DecodeCatalogMetadata::from_media_file(file)
            .map_err(|error| format!("decoder catalog facts are invalid: {error}"))?;
        #[cfg(test)]
        let fact_source =
            fact_source.with_final_identity_delay(self.decode_source_final_identity_delay);
        let facts = self
            .decode_facts
            .get_or_probe(
                probe,
                fact_source,
                Some(&catalog),
                crate::decode_facts::ProbeStreamSelection::LegacyVideoOrdinal(0),
                deadline
                    .saturating_duration_since(Instant::now())
                    .min(DECODE_PLAN_PROBE_BUDGET),
                cancelled,
            )
            .await;
        self.resolve_held_movie_plan_facts(file, options, encoder, facts)
            .await
    }

    pub(super) async fn resolve_held_movie_plan_facts(
        &self,
        file: &plurx_core::domain::MediaFile,
        options: &TranscodeOptions,
        encoder: Encoder,
        facts: Result<plurx_core::transcode::DecodeFacts, crate::decode_facts::DecodeFactError>,
    ) -> Result<ResolvedTranscode, String> {
        match facts {
            Ok(facts) => self.resolve_movie_plan_with_facts(
                file,
                options,
                encoder,
                &facts,
                &AttemptRestrictions::none(),
            ),
            // A probe that could not finish inside its budget, or a node with
            // no probe artifact at all, must not make the title unproducible.
            // Fall back to the same stored-probe plan live and offline already
            // use; the plan records `CatalogRow` so nothing downstream can
            // mistake it for a descriptor-bound measurement.
            Err(error) => {
                use crate::decode_facts::DecodePlanFallbackReason;

                let reason = error.fallback_reason();
                self.decode_facts.metrics().record_fallback(reason);
                match reason {
                    DecodePlanFallbackReason::Deadline | DecodePlanFallbackReason::Cancelled => {}
                    DecodePlanFallbackReason::ProbeChanged => {
                        if PROBE_CHANGED_WARNING_EMITTED
                            .compare_exchange(false, true, AcqRel, Acquire)
                            .is_ok()
                        {
                            tracing::warn!(
                                file_id = file.id,
                                reason = reason.label(),
                                "the configured ffprobe changed on disk after startup; bound facts are refused until plurxd restarts"
                            );
                        }
                    }
                    DecodePlanFallbackReason::RefusedSourceChanged => {
                        tracing::warn!(
                            file_id = file.id,
                            reason = reason.label(),
                            %error,
                            "bound decoder planning refused changed source facts"
                        );
                        return Err(
                            "the held source changed during decoder probing; rescan before playback"
                                .to_owned(),
                        );
                    }
                    DecodePlanFallbackReason::ProbeFailed
                    | DecodePlanFallbackReason::IdentityIo => {
                        tracing::warn!(
                            file_id = file.id,
                            reason = reason.label(),
                            %error,
                            "bound decoder planning fell back to stored probe facts"
                        );
                    }
                    DecodePlanFallbackReason::Invariant => {
                        tracing::error!(
                            file_id = file.id,
                            reason = reason.label(),
                            %error,
                            "bound decoder planning invariant failed; using stored probe facts"
                        );
                    }
                }
                tracing::debug!(
                    file_id = file.id,
                    reason = reason.label(),
                    %error,
                    "bound decoder planning fell back to stored probe facts"
                );
                self.resolve_movie_plan(file, options, encoder).await
            }
        }
    }

    /// Whether a plan describes the request it is sitting next to.
    ///
    /// Several call paths still carry `file` and `opts` alongside the plan
    /// because they need values the plan does not hold — the source path, the
    /// resume position, the scratch directory. Nothing checked that the two
    /// described the same work, which is the fault `Recipe::new` used to have
    /// one layer down: the key comes from the plan and the advertised shape
    /// from the options, so a mis-wired caller could look up one title and
    /// describe another. Cheap to check, and it fails closed.
    pub(super) fn plan_matches_request(
        plan: &ResolvedTranscode,
        file: &plurx_core::domain::MediaFile,
        opts: &TranscodeOptions,
    ) -> bool {
        plan.cache_identity() == &DecodeCacheIdentity::from_media_file(file)
            && plan.options().target_height == opts.target_height
            && plan.options().audio_index == opts.audio_index
            && plan.options().effective_rate_control == opts.effective_rate_control
    }

    /// The only constructor for a content-addressed transcode recipe.
    ///
    /// `opts` is already normalized and contains only validated effective rate
    /// control. Setting the encoder here prevents live lookup, speculative
    /// production, and offline production from drifting into different names
    /// for the same output. The manager-level pipeline remains byte-for-byte
    /// where the legacy recipe put it; changing that is a separate cache
    /// contract, not part of N1.
    pub(super) fn effective_recipe<'a>(
        &self,
        digest: &'a PipelineDigest,
        plan: &'a ResolvedTranscode,
        audio_copied: bool,
    ) -> Recipe<'a> {
        Recipe::new(digest, plan, audio_copied)
    }

    /// Which audio track a session carries, and which subtitle it burns.
    ///
    /// One function, called by the live path and by the producer, because the
    /// answer is part of the recipe: two sessions differing only in audio track
    /// are different bytes. A producer that skipped this and a playback that
    /// did it would compute two different names for the same film, and the
    /// symptom is a cache that fills forever and never hits — which looks
    /// exactly like a cache that is merely cold. It was, in fact, the first
    /// thing that went wrong when the producer was wired up.
    pub(super) async fn select_tracks(
        &self,
        file: &plurx_core::domain::MediaFile,
        audio_override: Option<i64>,
        subtitle_override: Option<i64>,
        base_is_hdr: bool,
    ) -> Tracks {
        let prefs = self.lang_prefs().await;
        Self::select_tracks_with_prefs(file, audio_override, subtitle_override, &prefs, base_is_hdr)
    }

    /// `base_is_hdr` is whether the session this request would build delivers
    /// HDR **without** a subtitle burn — see
    /// [`plurx_core::playback::burn_would_discard_hdr`]. It is what stops the
    /// implicit anime pick below tone-mapping a Dolby Vision remux to draw a
    /// subtitle nobody selected: the viewer never asked, so they never get the
    /// choice the clients' own HDR guard would have given them.
    pub(super) fn select_tracks_with_prefs(
        file: &plurx_core::domain::MediaFile,
        audio_override: Option<i64>,
        subtitle_override: Option<i64>,
        prefs: &plurx_core::tracks::LangPrefs,
        base_is_hdr: bool,
    ) -> Tracks {
        // Prefer original (Japanese) audio + subs when the file is dual-audio
        // anime-style (REQ-SUB-2), and honour the server-wide language
        // preferences otherwise. Native WebVTT-capable tracks remain media
        // renditions; only bitmap/styled fallbacks are drawn into the video.
        // The same predicate item detail, `/decision` and the offline quote
        // use. It folds language-tag case, which an inline copy here did not:
        // scanning stores `tags.language` exactly as the container wrote it,
        // so a `JPN` dual-audio file was advertised as Japanese by all three
        // of those surfaces and then played as English by this one.
        let prefer_original = plurx_core::tracks::prefers_original_audio(&file.audio_streams);
        // Unfiltered, deliberately. `deliverable_as_default` answers "would the
        // viewer see cues *without* a burn", and this is the one caller whose
        // job is to burn — REQ-SUB-2's whole point is that a dual-audio anime
        // release carries one ASS track and the server draws it. Handing that
        // predicate to the pick here would make `subtitle_index` skip every
        // ASS/SSA track, and the `subtitle_requires_burn` filter below would
        // strip whatever was left, so the anime rule would silently select
        // Japanese audio and no subtitles at all.
        let selection = plurx_core::tracks::select_tracks(
            &file.audio_streams,
            &file.subtitle_streams,
            prefer_original,
            prefs,
        );
        // A viewer's explicit choice wins over the automatic one, and is the
        // only way a bitmap subtitle is ever burned: the automatic rule exists
        // for dual-audio anime, where burning is a guess at what somebody
        // wants. Burning a subtitle nobody asked for is a picture they cannot
        // turn off.
        let burn_index = subtitle_override.filter(|i| *i >= 0).or_else(|| {
            prefer_original
                .then_some(selection.subtitle_index)
                .flatten()
                .filter(|idx| {
                    file.subtitle_streams
                        .get(*idx as usize)
                        .is_some_and(|s| plurx_core::tracks::subtitle_requires_burn(&s.codec))
                })
                // The guess may not cost the viewer their dynamic range. An
                // implicit burn on a delivery that would otherwise be HDR is
                // the M1 defect in this second place: nobody asked for the
                // subtitle, so nobody gets the choice the clients' own HDR
                // guard would have offered. Refused means no subtitle, never a
                // downgrade. An explicit override above is untouched — that
                // one the viewer did ask for, and session creation judges it.
                .filter(|_| !base_is_hdr)
        });
        Tracks {
            audio_index: audio_override.or(selection.audio_index),
            subtitle_burn: burn_index.and_then(|idx| {
                let codec = file
                    .subtitle_streams
                    .get(idx as usize)
                    .map(|s| s.codec.clone())?;
                Some(plurx_core::transcode::SubtitleBurn {
                    subtitle_index: idx,
                    bitmap: plurx_core::tracks::is_bitmap_subtitle(&codec),
                })
            }),
        }
    }

    /// The audio index a live session would carry for `file`, without starting
    /// one.
    ///
    /// Item detail, `/decision` and the offline quote all answer over HTTP, so
    /// a test can compare them directly; the session's own choice otherwise
    /// only becomes observable once ffmpeg is running. This exposes it so one
    /// regression can hold all four surfaces to a single track policy. Test
    /// visibility only — the recipe stays the caller's business in a build.
    #[cfg(test)]
    pub(crate) async fn session_audio_index(
        &self,
        file: &plurx_core::domain::MediaFile,
    ) -> Option<i64> {
        self.select_tracks(file, None, None, false)
            .await
            .audio_index
    }

    /// Whether this file needs RPU-driven reshaping rather than the ordinary
    /// HDR10 base-layer route. Unknown and unsupported non-compatible
    /// profiles are refused instead of being guessed through zscale.
    pub(super) fn needs_dovi_reshape(file: &plurx_core::domain::MediaFile) -> Result<bool, String> {
        if file.hdr.as_deref() != Some("dolby_vision") {
            return Ok(false);
        }
        if file.hdr_format.as_deref().is_some_and(|label| {
            label.contains("HDR10-compatible") || label.contains("HLG-compatible")
        }) {
            return Ok(false);
        }
        match plurx_core::playback::dolby_vision_profile(file) {
            Some(5) => Ok(true),
            Some(profile) => Err(unsupported_build_error(format!(
                "Dolby Vision Profile {profile} has no compatible base and no proven renderer; rescan after upgrading ffprobe or use a compatible source"
            ))),
            None => Err(unsupported_build_error(
                "Dolby Vision profile/base compatibility is unknown; rescan after upgrading ffprobe rather than producing untrusted colors",
            )),
        }
    }

    pub(super) fn dovi_proof_key(file: &plurx_core::domain::MediaFile) -> String {
        format!("{}:{}:{}", file.path.display(), file.size, file.mtime)
    }

    async fn require_dovi_renderer(
        &self,
        file: &plurx_core::domain::MediaFile,
    ) -> Result<bool, String> {
        let required = Self::needs_dovi_reshape(file)?;
        if required && !self.dovi_reshape {
            return Err(unsupported_build_error(
                "this ffmpeg did not prove the Dolby-aware tonemapx renderer required for Profile 5",
            ));
        }
        if required {
            let key = Self::dovi_proof_key(file);
            let cached = self
                .dovi_proofs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&key)
                .copied();
            let proved = match cached {
                Some(proved) => proved,
                None => {
                    let proved = crate::ffmpeg::dovi_reshape_changes_pixels(file).await;
                    self.dovi_proofs
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .insert(key, proved);
                    proved
                }
            };
            if !proved {
                return Err(unsupported_build_error("this source did not prove that Dolby Vision RPU application changes pixels through the production renderer"));
            }
        }
        Ok(required)
    }

    /// The grade this session will actually encode at.
    ///
    /// `requested` is the client's `hdr10t=1` carried through the create body.
    /// Everything else here is a refusal, and each one is load-bearing:
    ///
    /// - **Not asked for.** Absent means not proven (the wire contract), and
    ///   not proven means today's tone-mapped SDR rung.
    /// - **Not a source with an RPU to apply.** The passthrough filter is
    ///   Dolby-Vision-input-only: given a plain HDR10 source it emits a broken
    ///   picture — crushed shadows, clipping above roughly 70% — at exit 0,
    ///   with correct-looking tags (measured). The gate is real Dolby Vision
    ///   detection, never a config flag, and it is narrowed further to the
    ///   profiles the RPU renderer is actually built for
    ///   ([`plurx_core::playback::dolby_vision_needs_rpu_render`], which is
    ///   `needs_dovi_reshape` without the refusal messages).
    /// - **Not a measured encoder/geometry pair.** See [`HDR10_HEIGHT`] and
    ///   [`HDR10_4K_HEIGHT`].
    /// - **Not proved by this build**, or **not proved by this source**: the
    ///   boot probe says the graph runs, and `require_dovi_renderer` says
    ///   this file's RPU actually changes pixels through it. The second is the
    ///   one that cannot be inferred, and it is the same proof the SDR reshape
    ///   rung already demands.
    ///
    /// Every refusal degrades to [`OutputGrade::Sdr`], which is exactly
    /// today's behaviour — an HDR10 rung that cannot be proved is not an
    /// error, it is the ladder that already exists.
    pub(super) async fn hdr10_grade_for(
        &self,
        file: &plurx_core::domain::MediaFile,
        requested: bool,
        target_height: i64,
        encoder: Encoder,
        subtitle_burn: bool,
    ) -> Result<OutputGrade, String> {
        let Some(route) = plurx_core::playback::hdr_route(file) else {
            return Ok(OutputGrade::Sdr);
        };
        if !requested || !hdr10_rung_fits(file, target_height, encoder) {
            return Ok(OutputGrade::Sdr);
        }
        // A burn tone-maps. libass has no notion of a transfer function: it
        // draws nominal white at the CODE value, which on a BT.709 output is
        // diffuse white near 100 nits and on a PQ output is the top of the
        // curve — nominally 10,000. Subtitles that bright over an HDR picture
        // are not a styling complaint, they are painful to look at. Tone-map
        // instead and burn into the SDR frame, which is exactly what a burn on
        // an HDR title did before this rung existed.
        if subtitle_burn {
            tracing::info!(
                file = file.id,
                "a burned subtitle track tone-maps: subtitle white is a code value, and on a PQ \
                 output it lands at the top of the curve"
            );
            return Ok(OutputGrade::Sdr);
        }
        // A hardware encoder this rung was never measured on. Only libx265 and
        // hevc_qsv have Main10 routes here, so an NVENC or VA-API node would
        // fall back to software x265 — measured at 11 fps for 1080p on two
        // cores, under realtime, against the 20.3 fps the SDR chain it
        // replaced runs at. Losing the grade is a worse picture; losing
        // realtime is a stall, and the viewer notices that one.
        let preferred = self.encoder().await;
        if !matches!(preferred, Encoder::Software | Encoder::Qsv) {
            tracing::info!(
                file = file.id,
                encoder = preferred.label(),
                "this node's encoder has no measured Main10 route; tone-mapping to SDR rather \
                 than dropping to software x265"
            );
            return Ok(OutputGrade::Sdr);
        }
        if route == plurx_core::playback::HdrRoute::Passthrough {
            if !self.hdr10_passthrough {
                tracing::info!(
                    file = file.id,
                    "this ffmpeg did not prove the HDR10 passthrough encode; tone-mapping to SDR"
                );
                return Ok(OutputGrade::Sdr);
            }
            // The QSV half is proved on its own. `dovi_passthrough_qsv` looks
            // like it would do — it is the same P010 upload into `hevc_qsv`
            // Main10 — but it short-circuits on `tonemapx` declaring
            // `apply_dovi`, a jellyfin filter this route neither uses nor
            // needs. Reusing it would take the 4K rung away from every QSV
            // node running stock ffmpeg, with no log line naming the reason.
            if encoder == Encoder::Qsv && !self.hdr10_passthrough_qsv {
                tracing::info!(
                    file = file.id,
                    "this node did not prove the QSV Main10 encode for a plain HDR source; using \
                     the measured software/SDR route"
                );
                return Ok(OutputGrade::Sdr);
            }
            // No RPU to prove: the graph reads no metadata, so the boot proof
            // is the whole proof. `require_dovi_renderer` below is a
            // per-source check that this file's RPU actually changes pixels,
            // which is a question this route never asks.
            return Ok(OutputGrade::Hdr10);
        }
        if encoder == Encoder::Qsv && !self.dovi_passthrough_qsv {
            tracing::info!(
                file = file.id,
                "this node did not prove the Dolby Vision HDR10 QSV encode graph; using the measured software/SDR route"
            );
            return Ok(OutputGrade::Sdr);
        }
        if !self.dovi_passthrough {
            tracing::info!(
                file = file.id,
                "this ffmpeg did not prove the Dolby Vision HDR10 passthrough renderer; \
                 using the tone-mapped SDR rung"
            );
            return Ok(OutputGrade::Sdr);
        }
        // The same per-source proof the SDR reshape rung demands: that the
        // decoder really exports RPU side data for THIS file and that
        // applying it really changes pixels. A `hdr=dolby_vision` column is a
        // scan's opinion; this is a measurement.
        if !self.require_dovi_renderer(file).await? {
            return Ok(OutputGrade::Sdr);
        }
        Ok(OutputGrade::Hdr10)
    }

    /// The grade a session would deliver, without building one.
    ///
    /// A projection of [`Self::encoder_and_grade_for`] rather than a second
    /// resolver: the encoder and the grade are one measured route, and a
    /// preview that re-derived the grade on its own is exactly the drift seam
    /// `resolve_plan`'s doc warns about. Callers that only need the grade get
    /// the same answer `start` will, because it is literally the same call.
    ///
    /// `subtitle_burn: None` asks the question the HDR subtitle guard needs —
    /// *what would this session deliver with no burn?* — which is the only
    /// grade a burn may be judged against. Passing the burn here instead
    /// always answers `Sdr`, because a burn tone-maps by design.
    ///
    /// An encoder the node cannot resolve degrades to `Sdr`, the same way
    /// every refusal inside `hdr10_grade_for` does: a grade nobody can prove
    /// is not an HDR delivery, so a burn into it takes nothing away.
    pub(crate) async fn grade_preview(
        &self,
        file: &plurx_core::domain::MediaFile,
        requested_hdr10: bool,
        target_height: i64,
        subtitle_burn: Option<i64>,
    ) -> OutputGrade {
        self.encoder_and_grade_for(
            file,
            requested_hdr10,
            target_height,
            subtitle_burn.is_some_and(|index| index >= 0),
        )
        .await
        .map(|(_, grade)| grade)
        .unwrap_or(OutputGrade::Sdr)
    }

    /// Resolve grade and encoder together. The HDR filter and encoder are one
    /// measured route: choosing QSV through the ordinary SDR selector first
    /// could pair a PQ graph with H.264, while pinning every HDR request to
    /// software would throw away the node's proved 4K route.
    pub(super) async fn encoder_and_grade_for(
        &self,
        file: &plurx_core::domain::MediaFile,
        requested: bool,
        target_height: i64,
        subtitle_burn: bool,
    ) -> Result<(Encoder, OutputGrade), String> {
        let hdr_candidate = if requested
            && !subtitle_burn
            && plurx_core::playback::hdr_route(file).is_some()
            && matches!(target_height, HDR10_HEIGHT | HDR10_4K_HEIGHT)
        {
            let preferred = self.encoder().await;
            let qsv_proved = match plurx_core::playback::hdr_route(file) {
                Some(plurx_core::playback::HdrRoute::DolbyVisionRpu) => self.dovi_passthrough_qsv,
                _ => self.hdr10_passthrough_qsv,
            };
            if preferred == Encoder::Qsv && qsv_proved {
                Encoder::Qsv
            } else {
                Encoder::Software
            }
        } else {
            self.encoder_for_file(file).await?
        };
        let grade = self
            .hdr10_grade_for(file, requested, target_height, hdr_candidate, subtitle_burn)
            .await?;
        if grade == OutputGrade::Hdr10 {
            Ok((hdr_candidate, grade))
        } else {
            Ok((self.encoder_for_file(file).await?, grade))
        }
    }

    pub(super) async fn encoder_for_file(
        &self,
        file: &plurx_core::domain::MediaFile,
    ) -> Result<Encoder, String> {
        let requested = self
            .store
            .get_setting(keys::HWACCEL)
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        self.encoder_for_file_with_preference(file, &requested)
            .await
    }

    pub(super) async fn encoder_for_file_with_preference(
        &self,
        file: &plurx_core::domain::MediaFile,
        requested: &str,
    ) -> Result<Encoder, String> {
        if self.require_dovi_renderer(file).await? {
            // Software decode is forced by the pipeline itself
            // (`requires_software_decode`) — that is what preserves the RPU
            // AVFrame side data the tonemapx graph consumes. The ENCODER is
            // free to be the node's real one, provided that exact pairing has
            // been proved at boot; an unproved pairing falls back to software
            // rather than failing in front of a viewer. Pinning it to x264
            // used to cap 4K Dolby Vision at 720p for every non-DV client.
            let preferred = self.caps.choose(requested);
            if preferred != Encoder::Software
                && crate::ffmpeg::has_dovi_reshape_with(preferred).await
            {
                Ok(preferred)
            } else {
                Ok(Encoder::Software)
            }
        } else {
            Ok(self.caps.choose(requested))
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn options_for_tone_map(
        &self,
        encoder: Encoder,
        file: &plurx_core::domain::MediaFile,
        target_height: i64,
        start_seconds: f64,
        audio_index: Option<i64>,
        subtitle_burn: Option<plurx_core::transcode::SubtitleBurn>,
        software_threads: Option<u32>,
        tone_map: ToneMap,
        grade: OutputGrade,
    ) -> TranscodeOptions {
        let subtitle_file = self.subtitle_file(file, subtitle_burn.as_ref());
        let dovi_reshape = Self::needs_dovi_reshape(file).unwrap_or(false);
        let hdr10 = grade == OutputGrade::Hdr10;
        TranscodeOptions {
            target_height,
            software_threads,
            video_bitrate_kbps: bitrate_for_height(target_height),
            // Quality mode is an H.264 measurement. The node sweep that
            // produced the per-family CRF/QVBR defaults never ran against
            // x265, where the same number means a different bitrate, so the
            // HDR10 rung takes the bounded VBR every rung had before N1.
            effective_rate_control: if hdr10 {
                EffectiveRateControl::Vbr
            } else {
                self.effective_rate_control(encoder)
            },
            audio_index,
            start_seconds,
            // The HDR10 rung does not tone-map at all — that is what it is
            // for — so `ToneMap::None` is the honest recipe value. It is also
            // inert: `Pipeline::DoviPassthrough` owns the whole graph, and
            // `video_filters` returns before the tone-map branch is reached.
            tone_map: if hdr10 {
                ToneMap::None
            } else if dovi_reshape {
                ToneMap::Tonemapx
            } else {
                tone_map
            },
            // The node proved a graph; this session may still not be entitled
            // to it (HLG, non-compatible Dolby Vision, a light source, an
            // encoder it cannot feed). Deciding once, here,
            // is what keeps the log line honest — `pipeline=` is what actually
            // ran, not what the box is capable of. Routed by `routing_hdr`
            // rather than the raw column: a DV base layer that is
            // HDR10-compatible is an hdr10 stream to a tone-map, and a bitmap
            // subtitle burn keeps the GPU graph (it downloads once for
            // libass/overlay after the expensive scale + tone-map is done).
            pipeline: if hdr10 {
                // Admitted by `hdr10_grade_for`, which has already established
                // which renderer this source's HDR needs. The two are not
                // interchangeable: the Dolby graph applies an RPU and emits a
                // broken picture at exit 0 on ordinary PQ frames, and the
                // plain graph would render a Profile 5 base layer as garbage.
                // Nothing below this line may select either from a column
                // alone.
                match plurx_core::playback::hdr_route(file) {
                    Some(plurx_core::playback::HdrRoute::DolbyVisionRpu) => {
                        Pipeline::DoviPassthrough
                    }
                    Some(plurx_core::playback::HdrRoute::Passthrough) => Pipeline::Hdr10Passthrough,
                    // Not reachable: `hdr10_grade_for` answers Sdr for a
                    // source with no route, and `hdr10` is its answer. Spelled
                    // out rather than left to a catch-all, because a catch-all
                    // here would hand an SDR or HLG source the PQ graph.
                    None => Pipeline::Cpu,
                }
            } else if dovi_reshape {
                Pipeline::DoviTonemapx
            } else {
                Pipeline::for_session_with_scan(
                    self.pipeline,
                    encoder,
                    transcode::routing_hdr(file),
                    transcode::heavy_source(file),
                    subtitle_burn.as_ref().is_some_and(|b| !b.bitmap),
                    plurx_core::domain::ScanType::from_field_order(file.field_order.as_deref()),
                )
            },
            subtitle_burn,
            subtitle_file,
            // Only where the startup probe proved this build takes it. A
            // family that needs the flag and cannot have it still works; its
            // segments just follow the encoder's GOP, which is slower to start
            // and is logged as such at boot.
            force_idr: self.caps.forced_idr.wanted_by(encoder),
            ..Default::default()
        }
    }

    /// Normalize one path's inputs with the effective rate control that path
    /// captured. Keeping the final overwrite here is load-bearing: the base
    /// builder reads the manager's current snapshot, which may have changed
    /// since a live/speculative generation was captured and must never replace
    /// an offline package's durable value.
    #[allow(clippy::too_many_arguments)]
    fn options_for_effective_rate_control(
        &self,
        encoder: Encoder,
        file: &plurx_core::domain::MediaFile,
        target_height: i64,
        start_seconds: f64,
        audio_index: Option<i64>,
        subtitle_burn: Option<plurx_core::transcode::SubtitleBurn>,
        software_threads: Option<u32>,
        tone_map: ToneMap,
        effective_rate_control: EffectiveRateControl,
        grade: OutputGrade,
    ) -> TranscodeOptions {
        let mut opts = self.options_for_tone_map(
            encoder,
            file,
            target_height,
            start_seconds,
            audio_index,
            subtitle_burn,
            software_threads,
            tone_map,
            grade,
        );
        // Not for the HDR10 rung: `options_for_tone_map` pinned it to VBR
        // because x265 has no measured quality-mode setting, and a captured
        // snapshot must not put one back.
        if opts.pipeline.output_grade() == OutputGrade::Sdr {
            opts.effective_rate_control = effective_rate_control;
        }
        opts
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn live_lookup_options(
        &self,
        rate_control: RateControlSnapshot,
        encoder: Encoder,
        file: &plurx_core::domain::MediaFile,
        target_height: i64,
        start_seconds: f64,
        audio_index: Option<i64>,
        subtitle_burn: Option<plurx_core::transcode::SubtitleBurn>,
        software_threads: Option<u32>,
        grade: OutputGrade,
    ) -> TranscodeOptions {
        self.options_for_effective_rate_control(
            encoder,
            file,
            target_height,
            start_seconds,
            audio_index,
            subtitle_burn,
            software_threads,
            tone_map_pref(),
            rate_control.effective_for(encoder),
            grade,
        )
    }

    pub(super) fn speculative_producer_options(
        &self,
        rate_control: RateControlSnapshot,
        encoder: Encoder,
        file: &plurx_core::domain::MediaFile,
        target_height: i64,
        audio_index: Option<i64>,
        subtitle_burn: Option<plurx_core::transcode::SubtitleBurn>,
    ) -> TranscodeOptions {
        self.options_for_effective_rate_control(
            encoder,
            file,
            target_height,
            0.0,
            audio_index,
            subtitle_burn,
            None,
            tone_map_pref(),
            rate_control.effective_for(encoder),
            // Speculative pre-transcodes have no client to have proved a
            // Main10 PQ decoder, so they produce the SDR ladder — the rung
            // every client can take.
            OutputGrade::Sdr,
        )
    }

    pub(super) fn offline_package_options(
        &self,
        encoder: Encoder,
        file: &plurx_core::domain::MediaFile,
        spec: &OfflineSpec,
        subtitle_burn: Option<plurx_core::transcode::SubtitleBurn>,
    ) -> TranscodeOptions {
        self.options_for_effective_rate_control(
            encoder,
            file,
            spec.target_height,
            0.0,
            spec.audio_index,
            subtitle_burn,
            None,
            ToneMap::Zscale,
            spec.effective_rate_control,
            // An offline package is downloaded for playback anywhere later;
            // the SDR rung is the one that plays everywhere.
            OutputGrade::Sdr,
        )
    }

    /// A lossless-enough sidecar for simple text codecs. ASS/SSA remains
    /// embedded because converting it to WebVTT would discard positioning,
    /// styles, and the release's authored typography.
    fn subtitle_file(
        &self,
        file: &plurx_core::domain::MediaFile,
        burn: Option<&plurx_core::transcode::SubtitleBurn>,
    ) -> Option<PathBuf> {
        let burn = burn.filter(|burn| !burn.bitmap)?;
        let codec = file
            .subtitle_streams
            .get(burn.subtitle_index as usize)?
            .codec
            .to_lowercase();
        matches!(codec.as_str(), "subrip" | "srt" | "webvtt" | "mov_text")
            .then(|| PathBuf::from("/dev/fd/5"))
    }

    /// Materialise a text subtitle before ffmpeg opens the video pipeline.
    /// libass reopening the MKV itself reads the entire movie before frame one;
    /// the small cached VTT opens immediately and is shared with `/subs`.
    pub(super) async fn ensure_text_subtitle(
        &self,
        file: &plurx_core::domain::MediaFile,
        burn: Option<&plurx_core::transcode::SubtitleBurn>,
    ) -> Result<Option<std::fs::File>, String> {
        let Some(burn) = burn else {
            return Ok(None);
        };
        if self.subtitle_file(file, Some(burn)).is_none() {
            return Ok(None);
        }
        let stored = self.subtitle_source_access();
        crate::subtitles::ensure_vtt_file_with_store(
            &self.subtitle_cache,
            file,
            burn.subtitle_index,
            &stored,
        )
        .await
        .map(Some)
    }
}
