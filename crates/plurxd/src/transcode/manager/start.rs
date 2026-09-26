use super::*;

impl TranscodeManager {
    /// Start a transcode session for a file, superseding this viewer's previous
    /// session on the same file (see [`Self::reap_superseded_before`]).
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)] // one stream's worth of knobs
    pub async fn start(
        &self,
        file_id: i64,
        target_height: i64,
        start_seconds: f64,
        audio_override: Option<i64>,
        subtitle_override: Option<i64>,
        user_name: &str,
        playback_id: &str,
    ) -> Result<StartInfo, String> {
        let supersession_user = serde_json::json!(["username", user_name]).to_string();
        // A legacy process-local start carries no cluster identity, so no
        // budget. The ledger refuses an empty epoch, which is the answer.
        let recovery = SessionRecoveryIdentity {
            user_id: 0,
            incarnation_id: String::new(),
            recovery_epoch: String::new(),
        };
        self.start_with_audio_offset(
            file_id,
            target_height,
            start_seconds,
            audio_override,
            subtitle_override,
            0,
            user_name,
            &supersession_user,
            &recovery,
            None,
            None,
            playback_id,
            false,
            false,
            Priority::Live,
        )
        .await
    }

    /// Acquire the foreground encoder's durable permit after every background
    /// encoder has actually yielded.
    ///
    /// The waiter remains registered until the returned hardware/software
    /// permit exists, so there is no handoff gap in which a producer can take
    /// capacity back. The permit then carries live ownership for the session's
    /// whole lifetime, keeping every background pool parked after this method
    /// drops the short-lived waiter.
    pub(super) async fn admit_live(
        &self,
        preferred: Encoder,
        plan: Option<&ResolvedTranscode>,
        work: Workload<'_>,
        max_wait: Duration,
        priority: Priority,
    ) -> Result<LiveAdmission, String> {
        let _queued = (priority == Priority::Live).then(|| self.admissions.wait_for_slot());
        let deadline = Instant::now() + max_wait;
        let sw_budget = self.software_budget().await;

        if preferred != Encoder::Software {
            let max = self.max_hw_sessions().await;
            // What this pipeline will actually spend, read off the plan rather
            // than off the encoder's name. A hardware encoder fed by a software
            // decode spends most of a box's cores on the decode, and admitting
            // it as "hardware, therefore no CPU" is how several of them end up
            // on one node with every counter reading healthy.
            let estimate = plan.map(|plan| TranscodeResourceEstimate::of(plan, &work));
            let mixed =
                estimate.is_some_and(|estimate| estimate.hardware_slot && estimate.cpu_threads > 0);
            loop {
                let decision = self.admissions.admit_with_priority(max, work, priority);
                if let Admission::Hardware(slot) = decision {
                    if !mixed {
                        return Ok(LiveAdmission {
                            encoder: preferred,
                            hw_slot: Some(slot),
                            sw_permit: None,
                        });
                    }
                    // Release the slot before re-taking both together. The two
                    // pools are one mutex, so the bundle is granted whole or
                    // not at all; holding this slot while waiting for CPU is
                    // the shape that deadlocks against a start doing the same
                    // in the other order.
                    drop(slot);
                    let estimate = estimate.expect("a mixed pipeline has an estimate");
                    if let Some(bundle) = self
                        .admissions
                        .try_admit_bundle(max, sw_budget, &estimate, priority)
                    {
                        let (hw_slot, sw_permit) = bundle.into_parts();
                        tracing::info!(
                            target: "plurxd::transcode",
                            threads = estimate.cpu_threads,
                            "software decode into a hardware encoder; reserving the CPU it will spend"
                        );
                        return Ok(LiveAdmission {
                            encoder: preferred,
                            hw_slot,
                            sw_permit,
                        });
                    }
                    // A slot exists but the CPU the decode needs does not.
                    // Bounded, and answered honestly: the old accounting would
                    // have started here and simply not reserved the cores.
                    let now = Instant::now();
                    if now < deadline {
                        tokio::time::sleep(ADMISSION_POLL.min(deadline - now)).await;
                        continue;
                    }
                    let why = format!(
                        "a hardware slot is free but this title decodes in software and the CPU pool is spent ({} of {sw_budget} threads reserved); try again in a moment",
                        self.admissions.software_in_use()
                    );
                    tracing::warn!(
                        target: "plurxd::transcode",
                        class = %work.software_class(), "{why}"
                    );
                    return Err(capacity_error(why));
                }

                let now = Instant::now();
                if now < deadline {
                    tokio::time::sleep(ADMISSION_POLL.min(deadline - now)).await;
                    continue;
                }

                return match decision {
                    Admission::WaitingForBackground => Err(capacity_error(format!(
                        "background encoding did not yield within {:.1}s; try again in a moment",
                        max_wait.as_secs_f64()
                    ))),
                    Admission::Software => match self.admissions.try_admit_software(
                        sw_budget,
                        work.software_threads(),
                        priority,
                    ) {
                        Some(permit) => {
                            tracing::info!(
                                target: "plurxd::transcode",
                                class = %work.software_class(),
                                threads = permit.threads(),
                                "hardware transcode slots full; this class runs comfortably in software here, so starting it there"
                            );
                            Ok(LiveAdmission {
                                encoder: Encoder::Software,
                                hw_slot: None,
                                sw_permit: Some(permit),
                            })
                        }
                        None => {
                            let why = format!(
                                "all {max} hardware transcode slots are in use and the software CPU pool is spent ({} of {sw_budget} threads reserved); try again in a moment",
                                self.admissions.software_in_use()
                            );
                            tracing::warn!(
                                target: "plurxd::transcode",
                                class = %work.software_class(), "{why}"
                            );
                            Err(capacity_error(why))
                        }
                    },
                    Admission::Refused(why) => {
                        tracing::warn!(
                            target: "plurxd::transcode",
                            class = %work.software_class(), "{why}"
                        );
                        Err(capacity_error(why))
                    }
                    Admission::Hardware(_) => unreachable!("hardware returned above"),
                };
            }
        }

        loop {
            if let Some(permit) =
                self.admissions
                    .try_admit_software(sw_budget, work.software_threads(), priority)
            {
                return Ok(LiveAdmission {
                    encoder: Encoder::Software,
                    hw_slot: None,
                    sw_permit: Some(permit),
                });
            }
            let now = Instant::now();
            if now < deadline {
                tokio::time::sleep(ADMISSION_POLL.min(deadline - now)).await;
                continue;
            }
            let why = if self.admissions.background_is_active() {
                format!(
                    "background encoding did not yield within {:.1}s; try again in a moment",
                    max_wait.as_secs_f64()
                )
            } else {
                format!(
                    "the software CPU pool is spent ({} of {sw_budget} threads reserved) and no slot freed within {:.1}s; try again in a moment",
                    self.admissions.software_in_use(),
                    max_wait.as_secs_f64()
                )
            };
            tracing::warn!(target: "plurxd::transcode", class = %work.software_class(), "{why}");
            return Err(capacity_error(why));
        }
    }

    /// Reserve the foreground encoder pool for the measured live workload.
    /// Copy and audio-only routes never call this method, so an unavailable or
    /// saturated video encoder cannot block source-compatible playback.
    pub(crate) async fn admit_live_tv(
        &self,
        source_height: u16,
        codec: &str,
        hdr: Option<&str>,
        target_height: u16,
        max_wait: Duration,
    ) -> Result<LiveAdmission, String> {
        let preferred = self.encoder().await;
        let work = Workload {
            source_height: i64::from(source_height),
            codec,
            hdr,
            target_height: i64::from(target_height),
        };
        // Live TV plans its own command and is never served from the movie
        // cache, so there is no resolved movie plan to read an estimate from.
        self.admit_live(preferred, None, work, max_wait, Priority::Live)
            .await
    }

    #[allow(clippy::too_many_arguments)] // one stream's worth of knobs
    pub(super) async fn start_with_audio_offset(
        &self,
        file_id: i64,
        target_height: i64,
        start_seconds: f64,
        audio_override: Option<i64>,
        subtitle_override: Option<i64>,
        audio_offset_ms: i64,
        user_name: &str,
        supersession_user: &str,
        recovery: &SessionRecoveryIdentity,
        replacement_deadline: Option<tokio::time::Instant>,
        takeover: Option<SessionTakeoverStart>,
        playback_id: &str,
        automatic: bool,
        hdr10: bool,
        priority: Priority,
    ) -> Result<StartInfo, String> {
        let rate_control = self.rate_control_snapshot();
        // Cluster replacements are provisional until their durable pointer CAS
        // wins. Killing the old process here would turn an admission/Store
        // failure into an avoidable playback outage. Takeovers likewise
        // continue an existing incarnation and supersede nothing.
        if replacement_deadline.is_none() && takeover.is_none() {
            self.reap_superseded_before(None, supersession_user, playback_id)
                .await?;
        }

        let mut file = self
            .store
            .get_file(file_id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "file not found".to_owned())?;
        file.audio_offset_ms = if file.audio_streams.is_empty() {
            0
        } else {
            audio_offset_ms.clamp(-15_000, 15_000)
        };
        #[cfg(windows)]
        let source_handle = bind_windows_session_source(&mut file).await?;
        let item_title = self
            .store
            .get_item(file.item_id)
            .await
            .ok()
            .flatten()
            .map(|i| i.title)
            .unwrap_or_else(|| "(unknown)".to_owned());

        // Which tracks this session carries. Before the hardware slot, because
        // the cache lookup below needs the answer — two sessions differing only
        // in audio track are different bytes, and a cache that ignored that
        // would serve the wrong language.
        // The grade with no burn. Read before the tracks, because whether the
        // implicit pick may be a burn at all depends on what this session
        // would otherwise be delivering.
        let base_grade = self.grade_preview(&file, hdr10, target_height, None).await;
        let Tracks {
            audio_index,
            subtitle_burn,
        } = self
            .select_tracks(
                &file,
                audio_override,
                subtitle_override,
                base_grade == OutputGrade::Hdr10,
            )
            .await;

        // The cache, before anything is claimed. A hit needs no encoder, no
        // hardware slot and no place in the queue — the work is already done,
        // and making a viewer wait behind a busy GPU for bytes that exist is
        // the one thing this cache exists to prevent.
        // Resolved together, once, before the cache lookup: the grade and
        // encoder both change the bytes and therefore the recipe identity.
        let (mut encoder, grade) = self
            .encoder_and_grade_for(&file, hdr10, target_height, subtitle_burn.is_some())
            .await?;
        let mut opts = self.live_lookup_options(
            rate_control,
            encoder,
            &file,
            target_height,
            start_seconds,
            audio_index,
            subtitle_burn.clone(),
            None,
            grade,
        );
        if let Some(takeover) = takeover.as_ref() {
            opts.start_number = takeover.media_sequence;
        }
        let plan = self.resolve_movie_plan(&file, &opts, encoder).await?;
        if takeover.is_none() {
            if let Some(info) = self
                .serve_cached(
                    &file,
                    &opts,
                    &plan,
                    &item_title,
                    SessionOwner {
                        user_name,
                        supersession_user,
                        playback_id,
                        automatic,
                    },
                )
                .await
            {
                return Ok(info);
            }
        }
        // Can this ffmpeg build actually burn? Asked here — after the cache
        // lookup, which needs no ffmpeg at all, and before any slot, process
        // or session exists — because the alternative is spawning a graph that
        // dies at once with `No such filter` and reporting that to the viewer
        // as an anonymous stream failure a minute later. A build without the
        // filter will never grow one mid-session; refusing at the door names
        // the reason while the request is still in front of the person who
        // made it.
        if let Some(burn) = subtitle_burn.as_ref() {
            if let Some(reason) = crate::pipeprobe::burn_filters().await.refusal(burn.bitmap) {
                tracing::warn!(
                    target: "plurxd::transcode",
                    file = file_id,
                    subtitle = burn.subtitle_index,
                    bitmap = burn.bitmap,
                    "refusing a subtitle burn this ffmpeg build cannot run: {reason}"
                );
                return Err(unsupported_build_error(reason));
            }
        }
        let subtitle_handle = self
            .ensure_text_subtitle(
                &file,
                subtitle_burn.as_ref(),
                crate::process_control::ChildWork::realtime("text subtitle for a session start"),
            )
            .await?;

        // Claim a hardware slot before spawning anything. An iGPU has one
        // video-processing block, and a third 4K session on it does not run a
        // third as fast — it drags the other two under realtime with it, so one
        // person pressing play becomes three people stuttering.
        //
        // The wait is short and deliberate: a slot usually frees within seconds
        // (a superseded session, a closed tab), and someone who has pressed
        // play will forgive five seconds far sooner than a hang.
        let work = Workload::of(&file, target_height);
        let admission = self
            .admit_live(
                encoder,
                Some(&plan),
                work,
                if priority == Priority::Speculative {
                    Duration::ZERO
                } else {
                    QUEUE_WAIT
                },
                priority,
            )
            .await?;
        encoder = admission.encoder;
        if grade == OutputGrade::Hdr10
            && target_height == HDR10_4K_HEIGHT
            && encoder != Encoder::Qsv
        {
            return Err(capacity_error(
                "the proved 4K HDR10 QuickSync slot is busy; retry at 1080p",
            ));
        }
        let hw_slot = admission.hw_slot;
        let sw_permit = admission.sw_permit;
        let session_id = takeover
            .as_ref()
            .map(|takeover| takeover.provisional_session_id.clone())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        // The public UUID is a bearer capability. Keep it out of ffmpeg's
        // argv and stderr entirely by giving the scratch directory an
        // independent, process-private name.
        let dir = self.work_dir.join(format!("w-{}", uuid::Uuid::new_v4()));
        #[cfg(windows)]
        let dir = std::path::absolute(dir)
            .map_err(|error| format!("resolving Windows session directory: {error}"))?;
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|e| format!("creating session dir: {e}"))?;
        #[cfg(windows)]
        let output_handle = plurx_core::fs_secure::SecureDirectory::open(&dir)
            .await
            .map_err(|error| format!("holding Windows session directory: {error}"))?;
        let mut start_settlement = PrepublicationStartSettlement::new(dir.clone());

        // Admission may have moved this session to software, which changes the
        // pipeline it is entitled to — so the options are rebuilt rather than
        // patched. Same builder, so the two cannot describe different sessions.
        // The software permit's thread budget rides in the same rebuild: what
        // admission reserved is exactly what x264 is told to spend.
        let mut opts = self.live_lookup_options(
            rate_control,
            encoder,
            &file,
            target_height,
            start_seconds,
            audio_index,
            subtitle_burn,
            sw_permit.as_ref().map(|p| p.threads() as u32),
            grade,
        );
        if let Some(takeover) = takeover.as_ref() {
            opts.start_number = takeover.media_sequence;
        }
        #[cfg(windows)]
        if let Some(subtitle) = subtitle_handle.as_ref() {
            opts.subtitle_file = Some(
                plurx_core::fs_secure::std_file_path(subtitle)
                    .map_err(|error| format!("resolving held Windows subtitle path: {error}"))?,
            );
        }
        // Admission is allowed to demote the encoder. That is a different
        // byte-producing decision, so it receives a fresh complete plan;
        // neither the old encoder nor its decode surface is patched in place.
        let plan = self.resolve_movie_plan(&file, &opts, encoder).await?;
        // Every object FFmpeg's muxer writes for this session passes a
        // scratch grant before it reaches the disk, so the session starts on
        // its startup allowance and grows, instead of reserving the whole
        // per-session ceiling that an unbounded writer had to.
        let output_bitrate = transcode_output_bitrate(&opts);
        let scratch_envelope = rolling_scratch_envelope(output_bitrate, 1.0);
        let scratch_reservation = self
            .reserve_rolling_scratch(RollingScratchSizing::Startup(rolling_startup_bytes(
                output_bitrate,
                1.0,
            )))
            .await?;
        let upload = self.bind_scratch_upload(&dir, &scratch_reservation, scratch_envelope)?;
        let pacing = self.pacing(false).await;
        let automatic_decoder_recovery = self.automatic_decoder_recovery_enabled();
        let observation = DiagnosticObservation::for_plan(
            &plan,
            &self.measured_decoders,
            automatic_decoder_recovery,
        );
        let execution = TranscodeExecution::from_options(&file, &opts, pacing, &upload.base_url(0))
            .map_err(|error| error.to_string())?
            .observing_qualified_grammar(observation.qualified_logging());
        let args = transcode::hls_args(&plan, &execution);
        if plan.input_is_hdr()
            && plan.options().pipeline == Pipeline::Cpu
            && plan.options().tone_map == ToneMap::Zscale
        {
            crate::telemetry::record_tone_map_peak(plan.options().tone_map_peak_source);
        }
        // Log the exact command — the single most useful diagnostic. It reveals
        // the decode/filter/encode pipeline actually used (e.g. whether heavy
        // HEVC is being hardware-decoded), and confirms which build is running.
        //
        // And, when this session did not get the graph the node proved, why
        // not. Without it `pipeline=cpu` on a 4K HDR title reads as the GPU
        // path being broken, when the usual answer is that the source is Dolby
        // Vision and the CPU chain is the *correct* choice.
        let declined = Pipeline::declined_with_scan(
            self.pipeline,
            encoder,
            transcode::routing_hdr(&file),
            transcode::heavy_source(&file),
            opts.subtitle_burn.as_ref().is_some_and(|b| !b.bitmap),
            plurx_core::domain::ScanType::from_field_order(file.field_order.as_deref()),
        );
        tracing::info!(
            target: "plurxd::transcode",
            session = %session_log_id(&session_id), encoder = encoder.label(), pipeline = opts.pipeline.name(),
            proven = self.pipeline.name(), hdr = file.hdr.as_deref().unwrap_or("sdr"),
            peak_nits = plan.options().tone_map_peak_nits,
            peak_source = plan.options().tone_map_peak_source.name(),
            declined = declined.unwrap_or(""),
            deinterlace = plan.deinterlace().name(),
            build = crate::version::BUILD,
            "{}", ffmpeg_args_log_message("transcode ffmpeg args", &args, &session_id)
        );
        let session_kind = SessionKind::Transcode {
            height: target_height,
        };
        let hls_codecs = transcoded_hls_codecs(opts.pipeline.output_grade(), opts.target_height);
        let probe_json = match self.store.get_file_probe_json(file_id).await {
            Ok(probe_json) => probe_json,
            Err(error) => {
                let _ = tokio::fs::remove_dir_all(&dir).await;
                start_settlement.disarm();
                return Err(format!("reading frozen presentation probe: {error}"));
            }
        };
        let frozen_presentation = FrozenHlsPresentation::new(
            file.clone(),
            HlsContext {
                file_id,
                start_seconds,
                media_origin_seconds: start_seconds,
                codecs: hls_codecs.clone(),
                supplemental_codecs: None,
                frame_rate: frozen_video_frame_rate(probe_json.as_deref()),
            },
            &session_kind,
        );
        let presentation_contract_fingerprint = frozen_presentation.contract_fingerprint.clone();
        let retry = if encoder == Encoder::Software {
            None
        } else {
            // One read for both recipes: they are frozen together, and a
            // budget that moved between them would put the pair on two
            // different pictures of the node. Inside this branch rather than
            // above it, because a software encoder builds neither recipe and
            // has no reason to pay for a settings read on the start path.
            let software_budget = self.software_budget().await;
            let prepared = PrepublicationTranscodeRetry::prepare(
                &file,
                &opts,
                encoder,
                rate_control.effective_for(Encoder::Software),
            );
            let retry = match prepared {
                Ok(prepared) => match self
                    .resolve_movie_plan(&file, &prepared.opts, prepared.encoder)
                    .await
                {
                    Ok(retry_plan) => PrepublicationTranscodeRetry::build(
                        &file,
                        prepared,
                        &retry_plan,
                        pacing,
                        &upload.base_url(1),
                        &presentation_contract_fingerprint,
                        self.admissions.software_pool(),
                        software_budget,
                        self.runtime_cache.clone(),
                        &self.measured_decoders,
                        automatic_decoder_recovery,
                        "one-step-color-safe",
                    ),
                    Err(error) => Err(error),
                },
                Err(error) => Err(error),
            };
            match retry {
                Ok(retry) => {
                    // The software-decode alternate, frozen beside the
                    // colour-safe one and by the same rule: resolved now, from
                    // this plan, so that nothing re-runs policy in the middle
                    // of a recovery.
                    //
                    // A failure to build it is *not* a failure to start the
                    // session. The colour-safe retry is what this session has
                    // always had and it is unaffected; an absent alternate
                    // costs a decode fault its recovery and nothing else, and
                    // refusing to play a title because its hypothetical
                    // second recipe would not resolve is a worse trade than
                    // any recovery it buys.
                    let alternate = match self
                        .resolve_restricted_movie_plan(
                            &file,
                            &opts,
                            encoder,
                            &plurx_core::transcode::AttemptRestrictions::requiring(
                                plurx_core::transcode::DecodeBackend::Software,
                            ),
                        )
                        .await
                    {
                        Ok(alternate_plan) => {
                            PrepublicationTranscodeRetry::prepare_decode_restricted(
                                &plan,
                                &alternate_plan,
                                &opts,
                                encoder,
                            )
                            .and_then(|prepared| {
                                prepared
                                    .map(|prepared| {
                                        PrepublicationTranscodeRetry::build(
                                            &file,
                                            prepared,
                                            &alternate_plan,
                                            pacing,
                                            &upload.base_url(2),
                                            &presentation_contract_fingerprint,
                                            self.admissions.software_pool(),
                                            software_budget,
                                            self.runtime_cache.clone(),
                                            &self.measured_decoders,
                                            automatic_decoder_recovery,
                                            "software-decode",
                                        )
                                    })
                                    .transpose()
                            })
                            .unwrap_or_else(|error| {
                                tracing::warn!(
                                    target: "plurxd::transcode",
                                    session = %session_log_id(&session_id),
                                    "no software-decode alternate for this session: {error}"
                                );
                                None
                            })
                        }
                        Err(error) => {
                            tracing::warn!(
                                target: "plurxd::transcode",
                                session = %session_log_id(&session_id),
                                "software-decode alternate did not resolve: {error}"
                            );
                            None
                        }
                    };
                    // The durable budget, consulted once, here — and only to
                    // withhold.
                    //
                    // The actor chooses between the two frozen recipes, and it
                    // cannot ask the store: it is synchronous and holds no
                    // handle. So "has this playback already spent its one
                    // automatic recovery" is answered where a store and the
                    // identity are both in hand, by not giving the actor an
                    // alternate to name. A reopen of a playback that already
                    // recovered then reaches M5c1's permanent verdict on its
                    // first qualified fault — the honest terminal answer this
                    // effort exists to produce — instead of being told to
                    // retry and then failing at install.
                    //
                    // A row in *any* state withholds. `Reserved` is a recovery
                    // in flight or one abandoned by a cancelled executor, and
                    // both terminal states are a budget already spent.
                    let alternate = match (
                        alternate,
                        crate::playback_control::ProducerRecoveryLedger::new(
                            Arc::clone(&self.store),
                            recovery.user_id,
                            playback_id,
                            &recovery.recovery_epoch,
                            // The generation being started is the one that will
                            // fail, which is what a reservation records.
                            &recovery.incarnation_id,
                        ),
                    ) {
                        (Some(alternate), Some(ledger)) => match ledger.existing().await {
                            Ok(None) => Some(alternate.with_recovery(DecodeRecoveryReservation {
                                ledger,
                                failed_plan_digest: plan.plan_digest(),
                            })),
                            Ok(Some(existing)) => {
                                tracing::info!(
                                    target: "plurxd::transcode",
                                    session = %session_log_id(&session_id),
                                    state = existing.state.as_str(),
                                    "this playback has already used its recovery budget; \
                                     no software-decode alternate for this session"
                                );
                                None
                            }
                            Err(error) => {
                                // Fail closed. An unreadable budget is not an
                                // unspent one, and the cost of being wrong the
                                // other way is a second automatic recovery on
                                // a source that has already proven it does not
                                // decode.
                                tracing::warn!(
                                    target: "plurxd::transcode",
                                    session = %session_log_id(&session_id),
                                    %error,
                                    "the recovery budget could not be read; withholding the \
                                     software-decode alternate"
                                );
                                None
                            }
                        },
                        // No durable identity: a legacy process-local start, a
                        // relayed worker start, or a session predating the
                        // epoch column. Those keep the in-process one-shot they
                        // have always had — one automatic recovery per session
                        // — because withholding recovery from them would be a
                        // regression rather than a fix, and the reopen loop
                        // they can still reach is the loop that existed before
                        // this effort, not one it introduced.
                        (alternate, None) => alternate,
                        (None, _) => None,
                    };
                    Some(retry.with_decode_alternate(alternate))
                }
                Err(error) => {
                    let _ = tokio::fs::remove_dir_all(&dir).await;
                    start_settlement.disarm();
                    return Err(error);
                }
            }
        };
        let expected_remaining_ms =
            file.duration_ms
                .filter(|duration_ms| *duration_ms > 0)
                .map(|duration_ms| {
                    duration_ms
                        .saturating_sub((start_seconds * 1_000.0).round() as i64)
                        .max(0)
                });
        let completion_tolerance_ms = (transcode::SEGMENT_SECONDS as i64).saturating_mul(1_000);
        let policy = if let Some(retry) = retry.as_ref() {
            crate::playback_control::InitialProducerPolicy::hardware_with_startup(
                presentation_contract_fingerprint,
                PROGRESS_STALL,
                retry.actor_recipe.clone(),
                if plan.decode().backend() == plurx_core::transcode::DecodeBackend::Software {
                    crate::playback_control::ProducerStartupKind::MixedSoftwareDecode
                } else {
                    crate::playback_control::ProducerStartupKind::Hardware
                },
            )
            // The actor decides between the two; it can only do that if it can
            // see both. The executor holds the material either way, so an
            // alternate the policy did not name is one the actor will never
            // ask for and the executor will never install.
            .with_decode_alternate(
                retry
                    .decode_alternate
                    .as_deref()
                    .map(|alternate| alternate.actor_recipe.clone()),
            )
        } else {
            crate::playback_control::InitialProducerPolicy::software(
                presentation_contract_fingerprint,
                PROGRESS_STALL,
            )
        }
        .with_completion_expectation(expected_remaining_ms, completion_tolerance_ms);
        let progress = Arc::new(Progress::new());
        let (control, mut executor_registration) =
            crate::playback_control::RollingControlHandle::spawn_prepublication_transcode(
                "session-start",
            );

        let session = Arc::new(Session {
            dir: dir.clone(),
            response_incarnation: uuid::Uuid::new_v4(),
            frozen_presentation: Some(frozen_presentation),
            actor_managed_response_publication: true,
            actor_managed_prepublication_process: true,
            actor_prepublication_producer: Arc::new(AtomicBool::new(true)),
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
            cached: false,
            _cache_reader: None,
            subtitle_handle,
            #[cfg(windows)]
            source_handle: Some(source_handle),
            #[cfg(windows)]
            output_handle: Some(output_handle),
            cache_manifest: None,
            cache_location: None,
            control,
            publication: Mutex::new(RollingPublicationClock::default()),
            publication_worker_started: AtomicBool::new(false),
            flow_worker_started: AtomicBool::new(false),
            file_id,
            item_id: file.item_id,
            item_title,
            user_name: user_name.to_owned(),
            supersession_user: supersession_user.to_owned(),
            playback_id: playback_id.to_owned(),
            recovery: Some(recovery.clone()),
            automatic,
            kind: session_kind,
            method: crate::delivery::Method::Transcode,
            start_seconds,
            // A transcode seeks accurately, so its media begins exactly where
            // it was asked to: no probe, no discrepancy to resolve.
            media_origin_seconds: start_seconds,
            grade: opts.pipeline.output_grade(),
            target_height,
            tone_map_peak_nits: (plan.options().tone_map == ToneMap::Zscale)
                .then_some(plan.options().tone_map_peak_nits),
            tone_map_peak_source: (plan.options().tone_map == ToneMap::Zscale)
                .then_some(plan.options().tone_map_peak_source.name()),
            encoder_label: Mutex::new(encoder.label()),
            started_unix: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
            failed: Arc::new(AtomicBool::new(false)),
            failure: std::sync::Mutex::new(None),
            playlist_published: AtomicBool::new(false),
            high_segment: Arc::new(AtomicI64::new(-1)),
            compatibility_attempt: Arc::new(std::sync::Mutex::new(0)),
            fetched_end_ms: Arc::new(AtomicI64::new(0)),
            segments: Mutex::new(SegmentIndex::default()),
            ahead_bytes: AtomicI64::new(0),
            live_bytes: Arc::new(AtomicI64::new(0)),
            scratch: Some(scratch_reservation.bound_to(&session_id, 0)),
            retired_release: Arc::new(RetiredRelease::new()),
            scratch_envelope,
            upload: Some(upload),
            retention_garbage_bytes: Arc::new(AtomicI64::new(0)),
            retention_cleanup_queue: Arc::new(std::sync::Mutex::new(Vec::new())),
            retention_cleanup_active: Arc::new(AtomicBool::new(false)),
            progress: Arc::clone(&progress),
            class: std::sync::Mutex::new(work.class(if encoder == Encoder::Software {
                crate::admission::SOFTWARE
            } else {
                encoder.label()
            })),
            hw_slot: std::sync::Mutex::new(hw_slot),
            sw_permit: std::sync::Mutex::new(sw_permit),
            sw_delta_permit: std::sync::Mutex::new(None),
            delivery: Meter::for_method(crate::delivery::Method::Transcode.metric_label()),
            http_waits: HttpWaitLedger::default(),
            readrate: pacing
                .readrate
                .unwrap_or(if pacing.legacy_re { 1.0 } else { 0.0 }),
            suspended: AtomicBool::new(false),
            suspended_at: Mutex::new(None),
            suspend_count: AtomicU64::new(0),
            takeover,
            first_slide_logged: AtomicBool::new(false),
        });
        start_settlement.attach(&session);
        if let Err(reason) = executor_registration.register().await {
            fail_prepublication_transaction(
                &session,
                format!("rolling control actor rejected executor registration: {reason:?}"),
            )
            .await;
            start_settlement.disarm();
            return Err(format!(
                "rolling control actor rejected executor registration: {reason:?}"
            ));
        }
        let executor_retry = retry
            .clone()
            .map(|retry| PrepublicationRetry::Transcode(Box::new(retry)));
        let (executor_activation, manager_publication) = tokio::sync::oneshot::channel();
        spawn_prepublication_executor_owner(
            &session,
            executor_registration,
            manager_publication,
            executor_retry,
            session_id.clone(),
        );
        let generation = match session.control.begin_initial_producer_attempt(policy).await {
            Ok(generation) => generation,
            Err(reason) => {
                fail_prepublication_transaction(
                    &session,
                    format!(
                        "rolling control actor rejected the initial producer policy: {reason:?}"
                    ),
                )
                .await;
                start_settlement.disarm();
                return Err(format!(
                    "rolling control actor rejected the initial producer policy: {reason:?}"
                ));
            }
        };
        session.bind_retry_compatibility_attempt(generation).await;
        if let Err(reason) = session
            .spawn_and_install_prepublication_child(generation, || {
                spawn_ffmpeg(
                    &args,
                    crate::process_control::ChildWork::realtime("playback transcode"),
                    encoder.label(),
                    &session_id,
                    FfmpegProgressObserver::rolling(
                        Arc::clone(&progress),
                        generation,
                        session.control.clone(),
                    ),
                    &self.runtime_cache,
                    {
                        #[cfg(unix)]
                        let descriptors = FfmpegDescriptors::from_raw_fds(
                            None,
                            None,
                            session
                                .subtitle_handle
                                .as_ref()
                                .map(std::os::fd::AsRawFd::as_raw_fd),
                            false,
                        );
                        #[cfg(windows)]
                        let descriptors = windows_session_descriptors(&session)?;
                        descriptors
                    },
                    observation.clone(),
                )
            })
            .await
        {
            fail_prepublication_transaction(
                &session,
                format!("initial transcode producer could not be installed: {reason}"),
            )
            .await;
            start_settlement.disarm();
            return Err(reason);
        }
        tracing::info!(
            target: "plurxd::transcode",
            session = %session_log_id(&session_id), file_id, target_height, start_seconds,
            encoder = encoder.label(), "started actor-owned prepublication transcode session"
        );
        if let Err(reason) = self
            .register_session(&session_id, Arc::clone(&session), generation)
            .await
        {
            start_settlement.disarm();
            return Err(format!("rolling session registration rejected: {reason:?}"));
        }
        if executor_activation.send(()).is_err() {
            let reason = "prepublication executor ended before manager publication".to_owned();
            fail_prepublication_transaction(&session, reason.clone()).await;
            start_settlement.disarm();
            return Err(reason);
        }
        start_settlement.settle();
        self.emit_session_event(
            &session_id,
            &session,
            "session_start",
            SessionEventFields {
                extra: Some(serde_json::json!({ "cache": "miss" }).to_string()),
                ..SessionEventFields::default()
            },
        )
        .await;
        self.record_codec_qualification_session(
            encoder,
            opts.pipeline.output_grade(),
            Some(opts.pipeline),
        );

        Ok(StartInfo {
            playlist_url: format!("/api/v1/hls/{session_id}/index.m3u8"),
            session_id,
            duration_ms: file.duration_ms,
            start_seconds,
            media_origin_seconds: start_seconds,
            target_height,
            kind: SessionKind::Transcode {
                height: target_height,
            },
            encoder: encoder.label(),
            grade: opts.pipeline.output_grade(),
            vod: false,
            control_lease_timeout_ms: crate::playback_control::ROLLING_LEASE_TIMEOUT_MS,
        })
    }

    /// Start a **copy-video** HLS session: the source video is repackaged into
    /// HLS (fMP4 segments) untouched, and only the audio is transcoded when the
    /// client can't take it. This is the remux path for players whose `<video>`
    /// won't accept a progressive fragmented MP4 (Safari) but decode HEVC/HDR
    /// natively via HLS — so the original 4K stream is preserved instead of the
    /// error-fallback re-encoding it down to 720p. No hardware/software encoder
    /// ladder (nothing is encoded), just a fail-fast guard.
    #[cfg(test)]
    pub(super) async fn start_copy(
        &self,
        file_id: i64,
        start_seconds: f64,
        audio_override: Option<i64>,
        options: CopySessionOptions,
        user_name: &str,
        playback_id: &str,
    ) -> Result<StartInfo, String> {
        let supersession_user = serde_json::json!(["username", user_name]).to_string();
        // As above: no cluster identity, no budget.
        let recovery = SessionRecoveryIdentity {
            user_id: 0,
            incarnation_id: String::new(),
            recovery_epoch: String::new(),
        };
        self.start_copy_with_audio_offset(
            file_id,
            start_seconds,
            audio_override,
            0,
            options,
            user_name,
            &supersession_user,
            &recovery,
            None,
            None,
            playback_id,
            false,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)] // one stream's worth of knobs
    pub(super) async fn start_copy_with_audio_offset(
        &self,
        file_id: i64,
        start_seconds: f64,
        audio_override: Option<i64>,
        audio_offset_ms: i64,
        options: CopySessionOptions,
        user_name: &str,
        supersession_user: &str,
        recovery: &SessionRecoveryIdentity,
        replacement_deadline: Option<tokio::time::Instant>,
        takeover: Option<SessionTakeoverStart>,
        playback_id: &str,
        automatic: bool,
    ) -> Result<StartInfo, String> {
        let mut file = self
            .store
            .get_file(file_id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "file not found".to_owned())?;
        if matches!(file.video_codec.as_deref(), Some("hevc" | "h265"))
            && !unverified_hevc_copy_enabled(self.store.as_ref()).await?
        {
            return Err(vod_refusal_error(
                "hevc_configuration_unverified",
                "HEVC rolling copy is unverified; enable unverified HEVC copy in Settings → Developer to allow it",
            ));
        }
        // Same make-before-break rule as the transcode path. A takeover
        // continues an existing incarnation and supersedes nothing.
        if replacement_deadline.is_none() && takeover.is_none() {
            self.reap_superseded_before(None, supersession_user, playback_id)
                .await?;
        }

        let probe_json = crate::hevc_census::probe_json_for_copy(self.store.as_ref(), &file)
            .await
            .ok()
            .flatten();
        file.audio_offset_ms = if file.audio_streams.is_empty() {
            0
        } else {
            audio_offset_ms.clamp(-15_000, 15_000)
        };
        #[cfg(windows)]
        let source_handle = bind_windows_session_source(&mut file).await?;
        // Copy-video sessions and transcodes must interpret an omitted audio
        // override identically. `/decision` marks the shared-policy pick as
        // default, so silently falling back to ffmpeg's first stream here can
        // make the player show English while the session carries (for example)
        // an Italian container default. An explicit viewer choice still wins.
        // A copy session burns nothing — only the audio index is read here —
        // but the fact is reported honestly rather than defaulted, so the one
        // predicate cannot be handed a lie at any call site. A copy delivers
        // the source's own grade, which is `Sdr`'s row in
        // `delivered_dynamic_range` for an SDR file and the source grade for
        // an HDR one.
        let copy_delivers_hdr = plurx_core::playback::burn_would_discard_hdr(
            plurx_core::playback::delivered_dynamic_range(
                &file,
                plurx_core::playback::PlaybackMethod::Remux,
                options.preserve_dolby_vision,
                OutputGrade::Sdr,
            ),
            true,
        );
        let audio_index = self
            .select_tracks(&file, audio_override, None, copy_delivers_hdr)
            .await
            .audio_index;
        let item_title = self
            .store
            .get_item(file.item_id)
            .await
            .ok()
            .flatten()
            .map(|i| i.title)
            .unwrap_or_else(|| "(unknown)".to_owned());
        // Every object this session writes passes a Rust grant boundary
        // before it exists, so it may start small and grow. Sizing covers the
        // *effective* startup gate —
        // the rolling publication clock's runway, not just the copy writer's
        // own 12 s — plus one complete segment, plus the envelope, because a
        // session that cannot reach a published playlist has no client to
        // drain it and nothing to wait for.
        //
        // FFmpeg's own `-f hls` muxer, where this session uses it, is held
        // to the same rule through the upload endpoint bound below.
        let copy_bitrate = file
            .bitrate
            .filter(|rate| *rate > 0)
            .map(|rate| rate as f64);
        let scratch_envelope = rolling_scratch_envelope(copy_bitrate, 1.0);
        let scratch_reservation = self
            .reserve_rolling_scratch(RollingScratchSizing::Startup(rolling_startup_bytes(
                copy_bitrate,
                1.0,
            )))
            .await?;

        let session_id = takeover
            .as_ref()
            .map(|takeover| takeover.provisional_session_id.clone())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let dir = self.work_dir.join(format!("w-{}", uuid::Uuid::new_v4()));
        #[cfg(windows)]
        let dir = std::path::absolute(dir)
            .map_err(|error| format!("resolving Windows session directory: {error}"))?;
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|e| format!("creating session dir: {e}"))?;
        #[cfg(windows)]
        let output_handle = plurx_core::fs_secure::SecureDirectory::open(&dir)
            .await
            .map_err(|error| format!("holding Windows session directory: {error}"))?;
        let mut start_settlement = PrepublicationStartSettlement::new(dir.clone());
        // FFmpeg's own HLS muxer -- the legacy writer, a takeover, and the
        // legacy retry a segmenter session keeps in reserve -- uploads
        // through this endpoint, so its objects are granted before they land
        // exactly as the segmenter's are. Lane 0 is the initial attempt and
        // lane 1 the retry.
        let upload = self.bind_scratch_upload(&dir, &scratch_reservation, scratch_envelope)?;

        // An ffmpeg capability, read from the daemon's own record of which
        // ffmpeg it runs. It used to be read off the CACHE config — which
        // carries a copy of the same string — so a node with no cache
        // configured silently answered "no dovi_rpu" whatever it was running,
        // left the DV configuration in every remux, and had Chrome refuse the
        // stream Safari played fine.
        let have_dovi = self.dv_strippable();
        // The GOP-aware segmenter owns an in-process RPU rewrite, so a fresh
        // HEVC copy can deliver the requested P7 -> P8.1 conversion. Takeover
        // and the legacy muxer cannot, and still narrow to the HDR10 base via
        // `served_copy_options`.
        //
        // `options` is **shadowed** rather than read alongside the served
        // value, because reading the wrong one is the bug this exists to
        // prevent and it shipped once already: the returned `SessionKind` was
        // built from the asked-for options, and that is what the create
        // response's badge is computed from, so a stream this path stripped to
        // HDR10 was badged Dolby Vision. Shadowing makes that unspellable —
        // every later `options.` in this function is the served answer. `asked`
        // survives only to log the difference.
        let asked = options;
        let segmenting = takeover.is_none() && copyseg::supports(file.video_codec.as_deref());
        let options = if segmenting && asked.convert_dolby_vision {
            asked
        } else {
            served_copy_options(&file, asked)
        };
        let served = options;
        let preserve = options.preserve_dolby_vision;
        // Logged whenever EITHER field was given up, not only when a conversion
        // was asked for. `convert && !preserve` is the pair split the other
        // way, and a path that silently discarded a conversion is exactly as
        // worth a line as one that silently discarded a preservation.
        if asked.preserve_dolby_vision != options.preserve_dolby_vision
            || asked.convert_dolby_vision != options.convert_dolby_vision
        {
            tracing::info!(
                target: "plurxd::transcode",
                file_id,
                asked_preserve = asked.preserve_dolby_vision,
                asked_convert = asked.convert_dolby_vision,
                dual_layer = plurx_core::playback::dolby_vision_is_dual_layer(&file),
                served_preserve = preserve,
                "this copy cannot convert Dolby Vision, so it serves the source's base \
                 layer rather than a stream that names a profile it did not produce"
            );
        }
        let video_options = transcode::CopyVideoOptions::from_probe(
            &file,
            probe_json.as_deref(),
            have_dovi,
            preserve,
        )
        .with_dolby_vision_conversion(options.convert_dolby_vision);
        if video_options.promotes_parameter_sets() && takeover.is_some() {
            return Err(
                "this HEVC source requires GOP-aware init promotion and cannot use the legacy takeover muxer"
                    .to_owned(),
            );
        }
        let pacing = self.pacing(true).await;
        let legacy_args = |output: &str| match takeover.as_ref() {
            Some(takeover) => transcode::hls_copy_args_with_sequence(
                &file,
                start_seconds,
                audio_index,
                options.transcode_audio,
                pacing,
                video_options,
                takeover.media_sequence,
                &init_object_name(Some(takeover.owner_epoch)),
                output,
            ),
            None => transcode::hls_copy_args_with_dolby_vision(
                &file,
                start_seconds,
                audio_index,
                options.transcode_audio,
                pacing,
                video_options,
                output,
            ),
        };
        // Take over the cutting when the source is one whose keyframes can be
        // read (docs/streaming/SEGMENTER-PLAN.md). ffmpeg then writes one continuous
        // fragmented stream down a pipe and `copyseg` decides where the
        // segments end — in front of a keyframe no player will discard a
        // leading picture at. A structural Unsupported result may ask the
        // actor for the one frozen legacy retry; the reader never performs the
        // replacement itself.
        let initial_args = if segmenting {
            transcode::copy_pipe_args_with_dolby_vision(
                &file,
                start_seconds,
                audio_index,
                options.transcode_audio,
                pacing,
                video_options,
            )
        } else {
            legacy_args(&upload.base_url(0))
        };
        tracing::info!(
            target: "plurxd::transcode",
            session = %session_log_id(&session_id),
            file_id,
            start_seconds,
            mode = if segmenting { "segmenter" } else { "legacy" },
            build = crate::version::BUILD,
            "{}",
            ffmpeg_args_log_message("copy-video HLS ffmpeg args", &initial_args, &session_id)
        );

        // Freeze the achieved media origin before actor admission. The actor,
        // retry recipe, and response contract must describe one immutable
        // presentation even if the start future is later cancelled.
        let media_origin_seconds = probe_media_origin(&file.path, start_seconds).await;

        // `served`, not `options`, from here on: the playlist must advertise
        // the HDR10 base this path serves and the session record must not
        // claim a conversion that did not happen.
        let (hls_codecs, hls_supplemental_codecs) =
            copied_hls_codecs(&file, audio_index, served, probe_json.as_deref());
        let copy_kind = SessionKind::Copy {
            aac: served.transcode_audio,
            preserve_dolby_vision: served.preserve_dolby_vision,
            convert_dolby_vision: served.convert_dolby_vision,
        };
        let frozen_presentation = FrozenHlsPresentation::new(
            file.clone(),
            HlsContext {
                file_id,
                start_seconds,
                media_origin_seconds,
                codecs: hls_codecs.clone(),
                supplemental_codecs: hls_supplemental_codecs.clone(),
                frame_rate: frozen_video_frame_rate(probe_json.as_deref()),
            },
            &copy_kind,
        );
        let presentation_contract_fingerprint = frozen_presentation.contract_fingerprint.clone();
        let retry = build_prepublication_copy_retry(
            segmenting,
            video_options,
            legacy_args(&upload.base_url(1)),
            &presentation_contract_fingerprint,
            self.runtime_cache.clone(),
        );
        let expected_remaining_ms =
            file.duration_ms
                .filter(|duration_ms| *duration_ms > 0)
                .map(|duration_ms| {
                    duration_ms
                        .saturating_sub((media_origin_seconds * 1_000.0).round() as i64)
                        .max(0)
                });
        let completion_tolerance_ms = (transcode::SEGMENT_SECONDS as i64).saturating_mul(1_000);
        let policy = if segmenting {
            crate::playback_control::InitialProducerPolicy::copy(
                presentation_contract_fingerprint,
                PROGRESS_STALL,
                retry.as_ref().map(|retry| retry.actor_recipe.clone()),
            )
        } else {
            crate::playback_control::InitialProducerPolicy::copy_immediate(
                presentation_contract_fingerprint,
                PROGRESS_STALL,
            )
        }
        .with_completion_expectation(expected_remaining_ms, completion_tolerance_ms);
        let progress = Arc::new(Progress::new());
        let (control, mut executor_registration) =
            crate::playback_control::RollingControlHandle::spawn_prepublication_producer(
                "copy-session-start",
            );
        let failed = Arc::new(AtomicBool::new(false));
        if let Err(reason) = control
            .bind_response_publication_contract(
                frozen_presentation.contract_fingerprint.clone(),
                Arc::clone(&failed),
            )
            .await
        {
            return Err(format!(
                "rolling control actor rejected copy response-publication failure fencing: {reason:?}"
            ));
        }
        let session = Arc::new(Session {
            // A copy session encodes nothing; `session_delivered_dynamic_range`
            // reads its range off the source and `preserve_dolby_vision`.
            grade: OutputGrade::Sdr,
            dir: dir.clone(),
            response_incarnation: uuid::Uuid::new_v4(),
            frozen_presentation: Some(frozen_presentation),
            actor_managed_response_publication: true,
            actor_managed_prepublication_process: true,
            actor_prepublication_producer: Arc::new(AtomicBool::new(true)),
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
            cached: false,
            _cache_reader: None,
            subtitle_handle: None,
            #[cfg(windows)]
            source_handle: Some(source_handle),
            #[cfg(windows)]
            output_handle: Some(output_handle),
            cache_manifest: None,
            cache_location: None,
            control,
            publication: Mutex::new(RollingPublicationClock::default()),
            publication_worker_started: AtomicBool::new(false),
            flow_worker_started: AtomicBool::new(false),
            file_id,
            item_id: file.item_id,
            item_title,
            user_name: user_name.to_owned(),
            supersession_user: supersession_user.to_owned(),
            playback_id: playback_id.to_owned(),
            recovery: Some(recovery.clone()),
            automatic,
            kind: copy_kind,
            method: crate::delivery::Method::HlsCopy,
            start_seconds,
            media_origin_seconds,
            target_height: file.height.unwrap_or(0),
            tone_map_peak_nits: None,
            tone_map_peak_source: None,
            encoder_label: Mutex::new("copy"),
            started_unix: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
            failed,
            failure: std::sync::Mutex::new(None),
            playlist_published: AtomicBool::new(false),
            high_segment: Arc::new(AtomicI64::new(-1)),
            compatibility_attempt: Arc::new(std::sync::Mutex::new(0)),
            fetched_end_ms: Arc::new(AtomicI64::new(0)),
            segments: Mutex::new(SegmentIndex::default()),
            ahead_bytes: AtomicI64::new(0),
            live_bytes: Arc::new(AtomicI64::new(0)),
            scratch: Some(scratch_reservation.bound_to(&session_id, 0)),
            retired_release: Arc::new(RetiredRelease::new()),
            scratch_envelope,
            upload: Some(upload),
            retention_garbage_bytes: Arc::new(AtomicI64::new(0)),
            retention_cleanup_queue: Arc::new(std::sync::Mutex::new(Vec::new())),
            retention_cleanup_active: Arc::new(AtomicBool::new(false)),
            progress: Arc::clone(&progress),
            class: std::sync::Mutex::new(String::new()),
            hw_slot: std::sync::Mutex::new(None),
            sw_permit: std::sync::Mutex::new(None),
            sw_delta_permit: std::sync::Mutex::new(None),
            delivery: Meter::for_method(crate::delivery::Method::HlsCopy.metric_label()),
            http_waits: HttpWaitLedger::default(),
            readrate: pacing
                .readrate
                .unwrap_or(if pacing.legacy_re { 1.0 } else { 0.0 }),
            suspended: AtomicBool::new(false),
            suspended_at: Mutex::new(None),
            suspend_count: AtomicU64::new(0),
            takeover,
            first_slide_logged: AtomicBool::new(false),
        });
        start_settlement.attach(&session);
        if let Err(reason) = executor_registration.register().await {
            fail_prepublication_transaction(
                &session,
                format!("rolling control actor rejected copy executor registration: {reason:?}"),
            )
            .await;
            start_settlement.disarm();
            return Err(format!(
                "rolling control actor rejected copy executor registration: {reason:?}"
            ));
        }
        let (executor_activation, manager_publication) = tokio::sync::oneshot::channel();
        spawn_prepublication_executor_owner(
            &session,
            executor_registration,
            manager_publication,
            retry.clone().map(PrepublicationRetry::Copy),
            session_id.clone(),
        );
        let generation = match session.control.begin_initial_producer_attempt(policy).await {
            Ok(generation) => generation,
            Err(reason) => {
                fail_prepublication_transaction(
                    &session,
                    format!(
                        "rolling control actor rejected the initial copy producer policy: {reason:?}"
                    ),
                )
                .await;
                start_settlement.disarm();
                return Err(format!(
                    "rolling control actor rejected the initial copy producer policy: {reason:?}"
                ));
            }
        };
        session.bind_retry_compatibility_attempt(generation).await;
        let pipe_stdout = if segmenting {
            match session
                .spawn_and_install_prepublication_pipe_child(generation, || {
                    spawn_ffmpeg_pipe(
                        &initial_args,
                        crate::process_control::ChildWork::realtime("playback transcode"),
                        &session_id,
                        FfmpegProgressObserver::rolling(
                            Arc::clone(&progress),
                            generation,
                            session.control.clone(),
                        ),
                        &self.runtime_cache,
                        {
                            #[cfg(unix)]
                            let descriptors = FfmpegDescriptors::default();
                            #[cfg(windows)]
                            let descriptors = windows_session_descriptors(&session)?;
                            descriptors
                        },
                        DiagnosticObservation::copy(&session_id),
                    )
                })
                .await
            {
                Ok(stdout) => Some(stdout),
                Err(reason) => {
                    fail_prepublication_transaction(
                        &session,
                        format!("initial copy pipe producer could not be installed: {reason}"),
                    )
                    .await;
                    start_settlement.disarm();
                    return Err(reason);
                }
            }
        } else {
            if let Err(reason) = session
                .spawn_and_install_prepublication_child(generation, || {
                    spawn_ffmpeg(
                        &initial_args,
                        crate::process_control::ChildWork::realtime("playback copy HLS"),
                        "copy",
                        &session_id,
                        FfmpegProgressObserver::rolling(
                            Arc::clone(&progress),
                            generation,
                            session.control.clone(),
                        ),
                        &self.runtime_cache,
                        {
                            #[cfg(unix)]
                            let descriptors = FfmpegDescriptors::default();
                            #[cfg(windows)]
                            let descriptors = windows_session_descriptors(&session)?;
                            descriptors
                        },
                        DiagnosticObservation::copy(&session_id),
                    )
                })
                .await
            {
                fail_prepublication_transaction(
                    &session,
                    format!("initial direct copy producer could not be installed: {reason}"),
                )
                .await;
                start_settlement.disarm();
                return Err(reason);
            }
            None
        };
        if let Some(stdout) = pipe_stdout {
            // Every object this writer publishes is authorized before it
            // exists. That is what makes the smaller admission above safe: an
            // under-estimated envelope makes the writer wait for the budget
            // rather than overrun it.
            let grants = session.scratch.as_ref().map(|permit| {
                crate::copyseg::WriteGrants::new(
                    Arc::clone(permit.ledger()),
                    permit.key(),
                    Arc::clone(&self.scratch_cap),
                    scratch_envelope,
                )
                .with_starved_signal(Arc::clone(&self.scratch_starved))
            });
            spawn_copy_reader_owner(
                Arc::clone(&session),
                stdout,
                dir.clone(),
                session_id.clone(),
                generation,
                file.clone(),
                video_options,
                grants,
            );
        }
        tracing::info!(
            target: "plurxd::transcode",
            session = %session_log_id(&session_id),
            file_id,
            start_seconds,
            producer_attempt = generation,
            mode = if segmenting { "segmenter" } else { "legacy" },
            "started actor-owned prepublication copy session"
        );
        if let Err(reason) = self
            .register_session(&session_id, Arc::clone(&session), generation)
            .await
        {
            // Registration rejection owns synchronous/detached retirement;
            // do not race that exact settlement with this start guard.
            start_settlement.disarm();
            return Err(format!(
                "rolling copy session registration rejected: {reason:?}"
            ));
        }
        if executor_activation.send(()).is_err() {
            let reason = "copy producer executor ended before manager publication".to_owned();
            fail_prepublication_transaction(&session, reason.clone()).await;
            start_settlement.disarm();
            return Err(reason);
        }
        start_settlement.settle();
        self.emit_session_event(
            &session_id,
            &session,
            "session_start",
            SessionEventFields::default(),
        )
        .await;

        Ok(StartInfo {
            playlist_url: format!("/api/v1/hls/{session_id}/index.m3u8"),
            session_id,
            duration_ms: file.duration_ms,
            start_seconds,
            media_origin_seconds,
            target_height: file.height.unwrap_or(0),
            // `served`, not `options` — for the reason the strip-down above
            // gives. This is the value the create response's
            // `delivered_dynamic_range` is computed from, and it overrides
            // whatever `/decision` said, so `options`' `preserve` would badge
            // a stream this path stripped to HDR10 as Dolby Vision: the
            // session record, the playlist and the badge would disagree about
            // one delivery, and the badge is the one the viewer reads.
            kind: SessionKind::Copy {
                aac: served.transcode_audio,
                preserve_dolby_vision: served.preserve_dolby_vision,
                convert_dolby_vision: served.convert_dolby_vision,
            },
            encoder: "copy",
            // A copy session encodes nothing; its dynamic range is the
            // source's, read off `kind`/`preserve_dolby_vision`.
            grade: OutputGrade::Sdr,
            vod: false,
            control_lease_timeout_ms: crate::playback_control::ROLLING_LEASE_TIMEOUT_MS,
        })
    }
}
