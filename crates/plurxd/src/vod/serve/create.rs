use super::*;

/// Prepared media and its build gate; no viewer, driver demand or child is
/// created until the final attachment transaction consumes it.
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
        Box::pin(self.commit_vod_rendition(
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
        if !source.matches_assignment(assignment)
            || (encoding.is_some()
                != matches!(source.request().kind, SessionKind::Transcode { .. }))
            || source.request().subtitle_burn.is_some()
        {
            return Err(
                "Source VOD preparation requires its exact unburned recipe assignment".into(),
            );
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
        viewer: Option<crate::state::PlaybackViewerDemand>,
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
                viewer,
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
        viewer: Option<crate::state::PlaybackViewerDemand>,
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
                viewer,
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
        let prepared = Box::pin(self.prepare_vod_rendition(
            VodRecipeRequest {
                request: source.request(),
                encoding,
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

    async fn try_create_with_release_fence(
        &self,
        prepared: VodRecipeRequest<'_>,
        file: &MediaFile,
        settings: &VodSettings,
        attribution: VodAttribution<'_>,
        session_id: String,
        fences: VodCreateFences<'_>,
    ) -> Result<VodStart, String> {
        if let Some(viewer) = &fences.viewer {
            viewer.require_local_authority().map_err(str::to_owned)?;
        }
        let req = prepared.request;
        let prepared = self
            .prepare_vod_rendition(prepared, file, settings, &fences, None)
            .await?;
        self.commit_vod_rendition(
            req,
            file,
            settings,
            attribution,
            session_id,
            fences,
            prepared,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn commit_vod_rendition(
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

    pub(super) async fn prepare_vod_rendition(
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
            .attach_rendition(&key, &identity, index, recipe, duration_ms, settings)
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
        };
        let initial_cap = serve.shared.pool.global_cap();
        let settings = VodSettings {
            working_set_bytes: 8 << 30,
            completed_cache_bytes: 50 << 30,
            block_budget: Duration::from_secs(30),
            materialize_budget: Duration::from_secs(30),
            blocked_get_cap: initial_cap + 7,
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
                Some(viewer.clone()),
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
