//! The worker's verified first window and its original processed admission.
use super::*;
use crate::transcode::TranscodeManager;
use sha2::{Digest, Sha256};

pub(super) struct PreparedProcessedFel {
    trial: super::super::ProcessedUpgradeTrial,
    file: plurx_core::domain::MediaFile,
    duration_ms: i64,
    deferred_window: Option<(i64, i64)>,
    output_digest: [u8; 32],
    terminal: Option<AdoptedProcessedTerminal>,
}

impl PreparedProcessedFel {
    pub(super) fn requested_metadata_bytes(&self) -> Option<usize> {
        let context = self.trial.request.candidate_context.as_deref()?;
        let plan = context.planning_snapshot.as_ref()?;
        let query = context.catalog_query.as_ref()?;
        let caps = context.canonical_caps.as_ref()?;
        checked_caps([
            std::mem::size_of::<Self>(),
            AdoptedProcessedTerminal::requested_heap_bytes(),
            std::mem::size_of::<super::super::CandidateExecutionContext>(),
            std::mem::size_of::<plurx_core::store::PlaybackPlanningSnapshot>()
                + 2 * std::mem::size_of::<usize>(),
            self.trial.request.adaptive_request_base_heap_bytes()?,
            self.trial
                .candidate
                .planned_codec
                .as_ref()
                .map_or(0, String::capacity),
            context
                .display_target_note
                .as_ref()
                .map_or(0, String::capacity),
            self.file.requested_heap_bytes()?,
            planning_snapshot_requested_heap_bytes(plan)?,
            query.requested_heap_bytes()?,
            crate::media_sessions::selected_finite_device_caps_heap_bytes(caps)?,
            context.owner_node_id.as_ref().map_or(0, String::capacity),
            context.planning_binding.as_ref()?.source_digest.capacity(),
            context
                .selected_candidate
                .planned_codec
                .as_ref()
                .map_or(0, String::capacity),
        ])
    }

    pub(super) fn candidate(&self) -> &plurx_core::playback::candidate::QualityCandidate {
        &self.trial.candidate
    }

    pub(super) fn deferred_window(&self) -> Option<(i64, i64)> {
        self.deferred_window
    }

    pub(super) fn terminal(&self) -> Option<&AdoptedProcessedTerminal> {
        self.terminal.as_ref()
    }

    pub(super) fn take_terminal(&mut self) -> Option<AdoptedProcessedTerminal> {
        self.terminal.take()
    }

    pub(super) fn duration_ms(&self) -> i64 {
        self.duration_ms
    }

    pub(super) fn preview(
        &self,
        session_id: &str,
        serving_generation: u64,
    ) -> crate::media_sessions::RemoteStartResponse {
        let encoding = &self.trial.root.encoding;
        let height = encoding.options.target_height;
        crate::media_sessions::RemoteStartResponse {
            processed_dv_profile: Some(8),
            processed_fel: Some(true),
            retained_output: None,
            audio_delivery: self.trial.request.audio_delivery.clone(),
            session_id: session_id.to_owned(),
            playlist_url: format!("/api/v1/hls/{session_id}/index.m3u8"),
            duration_ms: Some(self.duration_ms),
            start_seconds: self.trial.request.start_seconds,
            media_origin_seconds: 0.0,
            target_height: height,
            kind: super::super::SessionKind::Transcode { height },
            encoder: encoding.delivered_encoder_label().to_owned(),
            grade: encoding.delivered_output_grade(),
            vod: true,
            control_lease_timeout_ms: crate::playback_control::VOD_LEASE_TIMEOUT_MS,
            activation_generation: Some(serving_generation),
        }
    }

    pub(super) fn binding(&self) -> Result<(String, String), String> {
        let nonce = self
            .trial
            .root
            .instance_nonce
            .ok_or("processed original root has no nonce")?;
        let mut handle = Sha256::new();
        handle.update(b"plurx:adaptive-processed-fel:v1");
        handle.update(nonce);
        handle.update(self.output_digest);
        Ok((
            hex::encode(handle.finalize()),
            hex::encode(self.output_digest),
        ))
    }

    pub(super) async fn validate_adopt(
        &self,
        manager: &TranscodeManager,
        deadline: std::time::Instant,
    ) -> Result<(), String> {
        let request = &self.trial.request;
        let context = request
            .candidate_context
            .as_deref()
            .ok_or("processed root lost its planning")?;
        manager
            .validate_candidate_planning_binding(request, Some(deadline.into()), None)
            .await?
            .ok_or("processed root has no current planning binding")?;
        manager
            .validate_processed_root(request, context, &self.file, &self.trial.root)
            .await
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Reuse the existing exact Store adoption and viewer attachment inputs."
    )]
    pub(super) async fn attach(
        &self,
        manager: &TranscodeManager,
        envelope: &crate::media_sessions::RemoteStartRequest,
        settings: &crate::vodserve::VodSettings,
        user_name: &str,
        session_id: String,
        authority: crate::serving_fence::ServingAuthority,
        serving_generation: u64,
        deadline: std::time::Instant,
    ) -> Result<crate::vodserve::VodStart, String> {
        let request = &self.trial.request;
        let root = &self.trial.root;
        let context = request
            .candidate_context
            .as_deref()
            .ok_or("processed root lost its planning")?;
        if envelope.candidate_id != Some(context.candidate_id)
            || envelope.request.request_id != request.request_id
            || envelope.request.playback_id != request.playback_id
        {
            return Err("processed Adopt changed its exact original intent".into());
        }
        self.validate_adopt(manager, deadline).await?;
        if let Some(facts) = manager.vod.hls_facts(&session_id).await {
            if facts
                .encoding
                .as_ref()
                .is_some_and(|encoding| Arc::ptr_eq(encoding, &root.encoding))
            {
                return Ok(crate::vodserve::VodStart {
                    session_id,
                    duration_ms: self.duration_ms,
                });
            }
            return Err("processed Adopt session already has another owner".into());
        }
        let admission = crate::vodserve::PreparedRootAdmission {
            reuse_scope: root.reuse_scope,
            reused_parent: None,
            instance_allowance: root.instance_allowance.clone(),
            instance_nonce: root.instance_nonce,
            encoding: Arc::clone(&root.encoding),
            permit: root.permit.clone(),
        };
        let prepared = crate::vodserve::VodRecipeRequest {
            split_soundtracks: Vec::new(),
            admission_priority: crate::admission::Priority::Live,
            root_admission: Some(admission),
            measured_candidate: None,
            retained_capture: crate::vodserve::RetainedOutputCapture::New,
            request,
            encoding: Some(Arc::clone(&root.encoding)),
            soundtrack: None,
            companion: None,
        };
        let item_title = format!("#{}", self.file.item_id);
        manager
            .vod
            .try_create_cluster_for_viewer(
                prepared,
                &self.file,
                settings,
                crate::vodserve::VodAttribution {
                    user_name,
                    item_title: &item_title,
                    supersession_user: user_name,
                },
                session_id,
                crate::vodserve::VodServingAdmission::new(authority, serving_generation, deadline),
                crate::state::PlaybackViewerDemand {
                    principal: envelope.principal.clone(),
                    playback_id: envelope.request.playback_id.clone(),
                },
            )
            .await
    }
}

impl TranscodeManager {
    pub(super) async fn prepare_adaptive_fel(
        &self,
        envelope: &crate::media_sessions::RemoteStartRequest,
        claim: &AttemptClaim,
        snapshot: plurx_core::store::PlaybackPlanningSnapshot,
        purpose: AdaptivePreparePurpose,
        cutoff: &plurx_core::cluster::clock::ContinuousDeadline,
        source_object: &str,
        unsettled_window: &std::sync::atomic::AtomicBool,
    ) -> Result<PreparedProcessedFel, String> {
        use plurx_core::transcode::dv_processing::DvDestination;

        let catalog = envelope
            .candidate_catalog
            .as_ref()
            .filter(|catalog| {
                catalog.candidate.processed_dv || purpose == AdaptivePreparePurpose::DeferredFel
            })
            .ok_or("the FEL request has no original catalog identity")?;
        let binding = crate::media_pool::PlanningBinding::from_snapshot(&snapshot);
        if binding != catalog.binding
            || snapshot.generation != claim.binding.planning_generation
            || crate::media_sessions::bounded_adaptive_digest(&snapshot.settings)?
                != claim.binding.settings_digest
        {
            return Err(
                "the processed worker's source or settings differ from the original request".into(),
            );
        }
        let mut query = catalog
            .validated_catalog_query(
                &envelope.request,
                envelope.source_size,
                envelope.source_mtime,
            )
            .ok_or("the processed worker's original catalog query is invalid")?;
        let decoder = envelope
            .decoder_caps
            .as_ref()
            .ok_or("the processed worker requires current decoder capabilities")?;
        query.caps.video = decoder.device_caps().video;
        query
            .validate()
            .map_err(|error| format!("invalid processed decoder capabilities: {error:?}"))?;
        if Self::processed_catalog_destination(&snapshot, &query) != Some(DvDestination::Profile81)
            || envelope.request.dv_processing_destination != Some(DvDestination::Profile81)
            || (purpose == AdaptivePreparePurpose::Create && envelope.request.start_seconds != 0.0)
        {
            return Err(
                "the worker cannot qualify this Profile 7 to Profile 8.1 FEL request".into(),
            );
        }
        if purpose == AdaptivePreparePurpose::DeferredFel && !catalog.candidate.processed_dv {
            let predecessor_id = claim
                .binding
                .expected_predecessor_incarnation_id
                .ok_or("cold deferred FEL has no original incumbent")?;
            let deadline = cutoff
                .monotonic_cutoff()
                .map_err(|error| error.to_string())?;
            let previous = tokio::time::timeout_at(
                deadline.into(),
                self.store.media_session_route_for_playback(
                    &claim.binding.principal,
                    &claim.binding.playback_id,
                ),
            )
            .await
            .map_err(|_| "the original incumbent could not be checked within the grant")?
            .map_err(|error| error.to_string())?
            .filter(|route| {
                route.incarnation_id == predecessor_id.to_string()
                    && Some(route.owner_epoch) == claim.binding.expected_predecessor_owner_epoch
                    && route.recovery_epoch == claim.binding.recovery_epoch.to_string()
                    && route.state == "active"
                    && route.publication_ready_at_ms == 0
                    && route.lease_expires_at_ms > crate::media_sessions::unix_ms()
            })
            .ok_or("the cold deferred FEL original incumbent is no longer active")?;
            let previous: crate::media_sessions::RemoteStartRequest =
                serde_json::from_str(&previous.recipe_json).map_err(|error| error.to_string())?;
            if !previous.is_valid()
                || previous.candidate_id != Some(catalog.candidate.id)
                || previous.source_size != envelope.source_size
                || previous.source_mtime != envelope.source_mtime
                || previous.request.file_id != envelope.request.file_id
                || previous.request.kind.copy_contract() != query.copy_contract
                || previous.presentation_target != envelope.presentation_target
                || previous.decoder_caps != envelope.decoder_caps
            {
                return Err("the cold deferred FEL intent changed its current incumbent".into());
            }
        }
        let mut file = snapshot.file.clone();
        file.audio_offset_ms = if file.audio_streams.is_empty() {
            0
        } else {
            envelope.request.audio_offset_ms.clamp(-15_000, 15_000)
        };
        let mut context = Self::candidate_context(&catalog.candidate);
        context.canonical_caps = Some(query.caps.clone());
        context.catalog_query = Some(query);
        context.planning_binding = Some(binding);
        context.planning_snapshot = Some(Arc::new(snapshot));
        context.owner_node_id = Some(claim.key.worker_node_id.clone());
        let mut request = envelope.request.clone();
        request.candidate_context = Some(Box::new(context));
        let recovery = super::super::SessionRecoveryIdentity {
            principal: claim.binding.principal.clone(),
            incarnation_id: claim.key.route_incarnation_id.to_string(),
            recovery_epoch: claim.binding.recovery_epoch.to_string(),
        };
        let mut trial = self
            .prepare_processed_upgrade_before(
                &claim.key.session_id.to_string(),
                &request,
                &recovery,
                true,
                Some((cutoff, unsettled_window)),
            )
            .await?
            .ok_or("the worker could not prepare the exact FEL recipe")?;
        if purpose == AdaptivePreparePurpose::DeferredFel {
            if let Some(context) = trial.request.candidate_context.as_mut() {
                // The incumbent remains a separate live owner. The peer may
                // attach only the verified processed trial from this grant.
                context.ordinary_request = None;
            }
        }
        let encoding = &trial.root.encoding;
        let authored_profile81 = matches!(
            &encoding.dv_processing,
            plurx_core::transcode::dv_processing::DvSelection::Selected(plan)
                if plan.destination() == DvDestination::Profile81 && plan.profile81_level().is_some()
        );
        let verified = trial
            .prepared_window
            .as_ref()
            .ok_or("processed preparation has no original verified window")?;
        let start_ms = (trial.request.start_seconds * 1000.0).round() as i64;
        let expected_delivery = if purpose == AdaptivePreparePurpose::DeferredFel {
            crate::media_sessions::deferred_fel_output_matches(
                &envelope.request,
                &catalog.candidate,
                &trial.candidate,
            )
        } else {
            crate::media_sessions::same_fel_output_contract(&catalog.candidate, &trial.candidate)
        };
        if !expected_delivery
            || !authored_profile81
            || encoding.source_object_version != source_object
            || !encoding.preserves_processed_dv()
            || encoding.processed_fel() != Some(true)
            || !(verified.start <= start_ms && start_ms < verified.end)
            || trial.root.reused_parent.is_some()
        {
            return Err("completed output differs from the requested FEL recipe".into());
        }
        cutoff
            .entry_remaining()
            .map_err(|error| error.to_string())?;
        let duration_ms = {
            let plan = encoding.grid.plan(
                file.duration_ms.ok_or("processed source has no duration")?,
                0,
            );
            (plan.duration_ticks().saturating_mul(1000) / u64::from(plan.timescale.max(1))) as i64
        };
        let output_digest = {
            let runtime = encoding
                .dv_runtime
                .as_ref()
                .ok_or("processed FEL has no runtime")?;
            let held = runtime
                .prepared
                .lock()
                .map_err(|_| "processed first window is uncertain")?;
            let (first, window) = held.as_ref().ok_or("processed first window is absent")?;
            if (purpose == AdaptivePreparePurpose::Create && *first != 0) || window.bytes.is_empty()
            {
                return Err("processed FEL first window is not available".into());
            }
            // The existing completed-window validator has checked authored
            // Profile 8.1 signaling, reconstructed FEL, fresh RPU and Main10
            // color/output. This is that receipt, still bound to these bytes.
            let counts = window
                .receipt
                .as_ref()
                .ok_or("processed first window has no verification receipt")?
                .counts();
            if counts.accepted == 0
                || counts.emitted != counts.accepted
                || counts.el_accepted != counts.accepted
                || counts.rpu_fresh_accepted != counts.accepted
                || counts.residual_dropped_frames != 0
            {
                return Err("processed FEL and RPU coverage is incomplete".into());
            }
            Sha256::digest(&window.bytes).into()
        };
        let deferred_window = (purpose == AdaptivePreparePurpose::DeferredFel)
            .then_some((verified.start, verified.end));
        if unsettled_window.load(std::sync::atomic::Ordering::Acquire) {
            return Err("the original verified FEL qualification remains physically held".into());
        }
        let terminal = AdoptedProcessedTerminal::register(&trial.root)?;
        Ok(PreparedProcessedFel {
            trial,
            file,
            duration_ms,
            deferred_window,
            output_digest,
            terminal: Some(terminal),
        })
    }
}

impl TranscodeManager {
    pub(crate) fn begin_adaptive_staged_adopt(
        self: &Arc<Self>,
        adoption: AdaptiveStagedAdoption,
        preview_json: String,
        authority: crate::serving_fence::ServingAuthority,
        deadline: std::time::Instant,
    ) -> Result<
        tokio::sync::oneshot::Receiver<Result<AdaptiveAdopted, ScalarAdaptiveDiagnostic>>,
        String,
    > {
        let stage = &adoption.preparation;
        let requested_bytes = checked_caps([
            control_binding_bytes(&adoption.binding).ok_or("staged binding capacity invalid")?,
            control_key_bytes(&adoption.key).ok_or("staged attempt capacity invalid")?,
            adoption.worker.node_id.capacity(),
            adoption.worker.planning_source_digest.capacity(),
            adoption.worker.selected_recipe_digest.capacity(),
            adoption.worker.prepared_handle_digest.capacity(),
            adoption.worker.output_digest.capacity(),
            stage
                .quality_cancellation_key
                .as_ref()
                .map_or(0, String::capacity),
            stage.incarnation_id.capacity(),
            stage.session_id.capacity(),
            stage.playback_id.capacity(),
            stage.expected_predecessor_incarnation_id.capacity(),
            stage.expected_predecessor_owner_node_id.capacity(),
            stage.request_fingerprint.capacity(),
            stage.owner_node_id.capacity(),
            stage.recipe_json.capacity(),
            stage.response_json.capacity(),
            preview_json.capacity(),
        ]);
        if preview_json.len() > crate::media_sessions::ADAPTIVE_PREVIEW_BYTES
            || adopted_terminal_requested_bytes(&adoption.binding, &adoption.key, &adoption.worker)
                .is_none_or(|bytes| bytes > attempt_root::FENCE_BYTES)
            || requested_bytes.is_none_or(|bytes| {
                bytes > crate::media_sessions::MAX_ADAPTIVE_ATTEMPT_REQUEST_BYTES
            })
        {
            return Err("staged Adopt exceeds its original charged control payload".into());
        }
        let entry = self
            .adaptive_attempts
            .get(adoption.key.attempt_id)
            .ok_or("staged Adopt has no original worker")?;
        if adoption.worker.worker_instance_id != entry.worker_instance_id()
            || adoption.worker.serving_generation != entry.serving_generation()
        {
            return Err("staged Adopt names another worker generation".into());
        }
        let admission = entry.admit_control().map_err(str::to_owned)?;
        let (send, receive) = tokio::sync::oneshot::channel();
        let original = entry.clone();
        let manager = Arc::clone(self);
        entry
            .spawn_control(admission, async move {
                let result = restart_guarded_adoption(
                    authority.clone(),
                    manager.adopt_prepared_fel_stage(
                        &original,
                        adoption,
                        &preview_json,
                        authority,
                        deadline,
                    ),
                )
                .await;
                let _ = send.send(
                    result
                        .map_err(ScalarAdaptiveDiagnostic::from)
                        .and_then(AdaptiveAdopted::from_start),
                );
            })
            .map_err(str::to_owned)?;
        Ok(receive)
    }

    async fn adopt_prepared_fel_stage(
        &self,
        entry: &AttemptRef,
        adoption: AdaptiveStagedAdoption,
        preview_json: &str,
        authority: crate::serving_fence::ServingAuthority,
        deadline: std::time::Instant,
    ) -> Result<crate::vodserve::VodStart, String> {
        let deadline = original_adopt_deadline(entry, deadline)?;
        if !authority.is_current(entry.serving_generation())
            || std::time::Instant::now() >= deadline
        {
            return Err("staged Adopt lost its original authority or cutoff".into());
        }
        let mut held = entry.state().await;
        let AttemptState::Rich(rich) = &mut *held else {
            return Err("staged Adopt no longer has its original prepared root".into());
        };
        let PreparedWorkerProduct::ProcessedFel(pending) = &rich.pending else {
            return Err("staged Adopt requires its original processed product".into());
        };
        let prepared = rich
            .prepared
            .as_ref()
            .ok_or("staged Adopt has no exact Prepared receipt")?;
        let stage = &adoption.preparation;
        if pending.deferred_window().is_none()
            || rich.fence.is_some()
            || rich.adoption.is_some()
            || rich.preview_json.as_deref() != Some(preview_json)
            || rich
                .staged_adoption
                .as_ref()
                .is_some_and(|old| old != &adoption)
            || prepared.binding != adoption.binding
            || prepared.key != adoption.key
            || prepared.worker != adoption.worker
            || stage.recipe_json != rich.selected_recipe_json
        {
            return Err("staged Adopt differs from the retained original attempt".into());
        }
        let route: crate::media_sessions::RemoteStartRequest =
            serde_json::from_str(&stage.recipe_json)
                .map_err(|error| format!("decoding staged FEL recipe: {error}"))?;
        let response: crate::http::hls::StartResponse = serde_json::from_str(&stage.response_json)
            .map_err(|error| format!("decoding staged FEL response: {error}"))?;
        let preview = pending.preview(&stage.session_id, entry.serving_generation());
        let enhancement = crate::http::hls::session_enhancement(
            Some(&pending.file),
            &preview.kind,
            preview.grade,
            preview.processed_dv_profile,
            preview.processed_fel,
            None,
            None,
            None,
        );
        if !route.is_valid()
            || route.candidate_id != response.quality_candidate_id
            || response.session_id != preview.session_id
            || response.playlist_url != preview.playlist_url
            || response.start_seconds != preview.start_seconds
            || response.duration_ms != preview.duration_ms
            || response.delivered_audio != preview.audio_delivery
            || response.height != preview.target_height
            || response.control
                != crate::playback_control::ControlBootstrap::new(
                    &stage.session_id,
                    &stage.incarnation_id,
                    adoption.key.owner_epoch,
                    crate::playback_control::VOD_LEASE_TIMEOUT_MS,
                )
            || enhancement.is_none()
            || crate::http::hls::deferred_fel_stage_enhancement(&stage.response_json) != enhancement
            || response.delivered_dynamic_range.as_deref() != Some("dolby_vision")
            || response.delivered_dolby_vision_profile != Some(8)
        {
            return Err("staged Adopt response changed the verified FEL delivery".into());
        }
        let user_id = adoption
            .binding
            .principal
            .local_user_id()
            .ok_or("staged Adopt requires a local viewer")?;
        let user = self
            .store
            .get_user(user_id)
            .await
            .map_err(|error| format!("reading staged viewer: {error}"))?
            .ok_or("the staged viewer no longer exists")?;
        pending.validate_adopt(self, deadline).await?;
        if !authority.is_current(entry.serving_generation())
            || std::time::Instant::now() >= deadline
        {
            return Err("staged Adopt no longer has its original serving window".into());
        }
        if rich.staged_adoption.is_none() {
            rich.staged_adoption = Some(adoption);
        }
        let original = rich
            .staged_adoption
            .as_ref()
            .expect("retained staged Adopt");
        if rich.settlement.is_none() {
            rich.settlement = Some(
                crate::media_sessions::AdaptiveSessionSettlementGuard::try_begin(
                    original.key.session_id,
                )
                .ok_or("the staged FEL owner cannot protect the original attachment")?,
            );
        }
        let committed = match self.store.stage_adaptive_winner(original).await {
            Ok(AttemptAdoption::Applied(committed) | AttemptAdoption::Replay(committed)) => {
                committed
            }
            Ok(AttemptAdoption::Refused(_)) => {
                return Err("staged Adopt was refused; original owner remains held".into())
            }
            Err(error) => {
                return Err(format!(
                    "staged Adopt outcome is unknown; original owner remains held: {error}"
                ))
            }
        };
        if committed.route.recipe_json != original.preparation.recipe_json
            || committed.route.session_id != original.preparation.session_id
            || committed.route.incarnation_id != original.preparation.incarnation_id
            || committed.route.owner_node_id != original.preparation.owner_node_id
            || committed.route.owner_epoch != original.key.owner_epoch
            || committed.route.state != "active"
            || committed.route.publication_ready_at_ms
                != plurx_core::domain::MEDIA_SESSION_PUBLICATION_BLOCKED
        {
            return Err("staged Adopt committed a different route".into());
        }
        rich.staged_attachment_started = true;
        let start = pending
            .attach(
                self,
                &route,
                &rich.settings,
                &user.username,
                original.preparation.session_id.clone(),
                authority,
                entry.serving_generation(),
                deadline,
            )
            .await?;
        if start.session_id != original.preparation.session_id
            || start.duration_ms != pending.duration_ms()
        {
            return Err("staged attachment differs from the original prepared root".into());
        }
        // The actual pending VOD owner now owns the verified window and credit.
        // The existing viewer commit alone will advance the playback pointer.
        Self::retain_adopted_terminal(entry, &mut held)?;
        Ok(start)
    }
}
