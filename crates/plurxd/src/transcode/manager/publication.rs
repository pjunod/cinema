use super::*;

impl TranscodeManager {
    /// Admit a fully prepared response against the exact owner that resolved
    /// its bytes. The registry identity is checked on both sides of actor
    /// admission so a replacement cannot take the reusable session id during
    /// the await and receive its predecessor's response.
    pub(crate) async fn authorize_response_publication(
        self: &Arc<Self>,
        session_id: &str,
        owner: &MediaResponseOwner,
        publication: MediaResponsePublication,
        deadline: Instant,
    ) -> Result<MediaResponseAuthorization, MediaResponsePublicationRejection> {
        if tokio::time::Instant::now().into_std() >= deadline {
            return Err(MediaResponsePublicationRejection::StateChanged);
        }
        let release_gate = self
            .session_adoption_gate(session_id)
            .ok_or(MediaResponsePublicationRejection::StateChanged)?;
        if release_gate.released.load(Acquire) {
            return Err(MediaResponsePublicationRejection::StateChanged);
        }
        let admitted_serving_generation = self
            .serving_authority
            .admit()
            .ok_or(MediaResponsePublicationRejection::StateChanged)?;
        macro_rules! require_publication_authority {
            () => {
                if !self.publication_authority_is_current(admitted_serving_generation)
                    || release_gate.released.load(Acquire)
                {
                    return Err(MediaResponsePublicationRejection::StateChanged);
                }
            };
        }
        if let MediaResponseOwnerKind::Rolling {
            session,
            producer_attempt,
        } = &owner.0
        {
            require_publication_authority!();
            let current = tokio::time::timeout_at(
                tokio::time::Instant::from_std(deadline),
                self.sessions.lock(),
            )
            .await
            .map_err(|_| MediaResponsePublicationRejection::StateChanged)?
            .get(session_id)
            .cloned();
            require_publication_authority!();
            if !current
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, session))
            {
                let object = publication.rolling_object();
                let promised_object = matches!(
                    object,
                    crate::playback_control::RollingResponseObject::InitializationSegment
                        | crate::playback_control::RollingResponseObject::MediaSegment
                        | crate::playback_control::RollingResponseObject::ByteRange
                        | crate::playback_control::RollingResponseObject::NotModified
                        | crate::playback_control::RollingResponseObject::RangeNotSatisfiable
                );
                if promised_object
                    && matches!(
                        publication.binding,
                        MediaResponsePublicationBinding::AttemptMedia
                    )
                    && self
                        .retired_object_is_current(
                            session_id,
                            session,
                            *producer_attempt,
                            publication.object_name.as_deref(),
                        )
                        .await
                {
                    return Ok(MediaResponseAuthorization {
                        session_id: session_id.to_owned(),
                        owner: owner.clone(),
                        release_gate: Arc::clone(&release_gate),
                        admitted_serving_generation,
                        kind: publication.kind,
                        object_name: publication.object_name,
                        rolling_generation_metadata_fingerprint: None,
                        scratch_pin: None,
                    });
                }
                return Err(MediaResponsePublicationRejection::OwnerGone);
            }
            if !session.actor_managed_response_publication {
                if session.control.current_producer_attempt() != *producer_attempt
                    || session.control.is_retired()
                    || !self.publication_authority_is_current(admitted_serving_generation)
                {
                    return Err(MediaResponsePublicationRejection::StateChanged);
                }
                return Ok(MediaResponseAuthorization {
                    session_id: session_id.to_owned(),
                    owner: owner.clone(),
                    release_gate: Arc::clone(&release_gate),
                    admitted_serving_generation,
                    kind: publication.kind,
                    object_name: publication.object_name,
                    rolling_generation_metadata_fingerprint: None,
                    scratch_pin: None,
                });
            }
            // Keep all actor-managed publication calls for this Session
            // behind the exact first-media handoff. This is per generation,
            // not the global registry lock: a concurrent second response
            // cannot emit while the actor has closed prepublication but its
            // lifetime-ownership handoff is not yet process-locally visible.
            require_publication_authority!();
            let _response_transition = tokio::time::timeout_at(
                tokio::time::Instant::from_std(deadline),
                session.response_publication_transition.lock(),
            )
            .await
            .map_err(|_| MediaResponsePublicationRejection::StateChanged)?;
            require_publication_authority!();
            require_publication_authority!();
            let current = tokio::time::timeout_at(
                tokio::time::Instant::from_std(deadline),
                self.sessions.lock(),
            )
            .await
            .map_err(|_| MediaResponsePublicationRejection::StateChanged)?
            .get(session_id)
            .cloned();
            require_publication_authority!();
            if !current
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, session))
            {
                return Err(MediaResponsePublicationRejection::OwnerGone);
            }
            if session.control.is_retired() {
                return Err(MediaResponsePublicationRejection::StateChanged);
            }
            let object = publication.rolling_object();
            let media_segment_index = publication.rolling_media_segment_index(object);
            let mut rolling_generation_metadata_fingerprint = None;
            let actor_publication = match publication.binding {
                MediaResponsePublicationBinding::GenerationMetadata => {
                    let frozen = session
                        .frozen_presentation
                        .as_ref()
                        .ok_or(MediaResponsePublicationRejection::StateChanged)?;
                    if frozen
                        .sealed_stable_master_contract
                        .as_ref()
                        .is_some_and(|sealed| sealed == &frozen.contract_fingerprint)
                    {
                        rolling_generation_metadata_fingerprint =
                            frozen.sealed_stable_master_contract.clone();
                        crate::playback_control::RollingResponsePublication::generation_metadata(
                            object,
                            frozen.contract_fingerprint.clone(),
                        )
                    } else {
                        crate::playback_control::RollingResponsePublication::attempt_media(
                            object,
                            *producer_attempt,
                            media_segment_index,
                        )
                    }
                }
                MediaResponsePublicationBinding::AttemptMedia => {
                    crate::playback_control::RollingResponsePublication::attempt_media(
                        object,
                        *producer_attempt,
                        media_segment_index,
                    )
                }
                MediaResponsePublicationBinding::AttemptStatus => {
                    crate::playback_control::RollingResponsePublication::attempt_status(
                        object,
                        *producer_attempt,
                    )
                }
                MediaResponsePublicationBinding::ProtocolOnly => {
                    crate::playback_control::RollingResponsePublication::protocol_only(
                        object,
                        *producer_attempt,
                    )
                }
            };
            let attempt_media_publication = matches!(
                &actor_publication.binding,
                crate::playback_control::RollingResponsePublicationBinding::AttemptMedia { .. }
            );
            if attempt_media_publication
                && session.actor_prepublication_producer.load(Acquire)
                && matches!(&session.kind, SessionKind::Copy { .. })
            {
                match validate_copy_init_before_publication(session, *producer_attempt, deadline)
                    .await
                {
                    Ok(layout) => {
                        let description_count = match layout {
                            plurx_core::fmp4::HevcSampleEntryLayout::NotHevc => 0,
                            plurx_core::fmp4::HevcSampleEntryLayout::Single => 1,
                            plurx_core::fmp4::HevcSampleEntryLayout::Multiple { count } => count,
                        };
                        tracing::info!(
                            session = %session_log_id(session_id),
                            file_id = session.file_id,
                            start_seconds = session.start_seconds,
                            producer_attempt,
                            description_count,
                            build = crate::version::BUILD,
                            "validated copy init before first media handoff"
                        );
                    }
                    Err(CopyInitValidationError::StateChanged) => {
                        return Err(MediaResponsePublicationRejection::StateChanged);
                    }
                    Err(CopyInitValidationError::Invalid(reason)) => {
                        let failure = format!("copy output validation failed: {reason}");
                        fail_prepublication_transaction(session, failure.clone()).await;
                        return Err(MediaResponsePublicationRejection::ProducerEnded(failure));
                    }
                }
                require_publication_authority!();
            }
            let (actor_handoff, first_media_applied) = if attempt_media_publication
                && session.actor_prepublication_producer.load(Acquire)
            {
                require_publication_authority!();
                let Some((handoff, applied)) =
                    begin_first_media_publication_handoff_before(session, session_id, deadline)
                        .await
                else {
                    return Err(MediaResponsePublicationRejection::StateChanged);
                };
                require_publication_authority!();
                (Some(handoff), Some(applied))
            } else {
                (None, None)
            };
            require_publication_authority!();
            let actor_authorization = session
                .control
                .authorize_response_publication(actor_publication, actor_handoff, deadline)
                .await;
            let actor_authorization = match actor_authorization {
                Ok(authorization) => authorization,
                Err(_) => {
                    // Authorization is the final byte-publication fence. Only
                    // after it rejects may a second exact actor snapshot
                    // classify an already-open numeric segment as beyond the
                    // immutable retained frontier. This preserves the typed
                    // ProducerEnded response without a stale pre-check ever
                    // replacing actor authorization.
                    if attempt_media_publication {
                        if let Some(requested_segment) = media_segment_index {
                            require_publication_authority!();
                            if let Some((PlaylistError::ProducerEnded(reason), published_segment)) =
                                session
                                    .published_producer_ended_before(*producer_attempt, deadline)
                                    .await
                            {
                                require_publication_authority!();
                                if producer_request_beyond_frontier(
                                    Some(requested_segment),
                                    published_segment,
                                ) {
                                    let current = tokio::time::timeout_at(
                                        tokio::time::Instant::from_std(deadline),
                                        self.sessions.lock(),
                                    )
                                    .await
                                    .map_err(|_| MediaResponsePublicationRejection::StateChanged)?
                                    .get(session_id)
                                    .cloned();
                                    require_publication_authority!();
                                    if !current
                                        .as_ref()
                                        .is_some_and(|current| Arc::ptr_eq(current, session))
                                    {
                                        return Err(MediaResponsePublicationRejection::OwnerGone);
                                    }
                                    return Err(MediaResponsePublicationRejection::ProducerEnded(
                                        reason,
                                    ));
                                }
                            }
                        }
                    }
                    return Err(MediaResponsePublicationRejection::StateChanged);
                }
            };
            require_publication_authority!();
            if actor_authorization.first_producer_media_publication {
                let Some(applied) = first_media_applied else {
                    return Err(MediaResponsePublicationRejection::StateChanged);
                };
                require_publication_authority!();
                if !tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), applied)
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .unwrap_or(false)
                {
                    return Err(MediaResponsePublicationRejection::StateChanged);
                }
                require_publication_authority!();
            }
            if attempt_media_publication && session.actor_prepublication_producer.load(Acquire) {
                return Err(MediaResponsePublicationRejection::StateChanged);
            }
            if attempt_media_publication && session.actor_managed_prepublication_process {
                loop {
                    if session.first_media_handoff_applied.load(Acquire) {
                        break;
                    }
                    let applied = session.first_media_handoff_notify.notified();
                    if session.first_media_handoff_applied.load(Acquire) {
                        break;
                    }
                    require_publication_authority!();
                    if tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), applied)
                        .await
                        .is_err()
                    {
                        return Err(MediaResponsePublicationRejection::StateChanged);
                    }
                    require_publication_authority!();
                }
            }
            require_publication_authority!();
            let current = tokio::time::timeout_at(
                tokio::time::Instant::from_std(deadline),
                self.sessions.lock(),
            )
            .await
            .map_err(|_| MediaResponsePublicationRejection::StateChanged)?
            .get(session_id)
            .cloned();
            require_publication_authority!();
            if !current
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, session))
            {
                return Err(MediaResponsePublicationRejection::OwnerGone);
            }
            if object == crate::playback_control::RollingResponseObject::VideoMediaPlaylist && {
                require_publication_authority!();
                !tokio::time::timeout_at(
                    tokio::time::Instant::from_std(deadline),
                    session.publish_compatibility_playlist(*producer_attempt, deadline),
                )
                .await
                .unwrap_or(false)
            } {
                require_publication_authority!();
                fail_prepublication_transaction(
                    session,
                    "actor-authorized playlist could not enter the exact compatibility attempt"
                        .to_owned(),
                )
                .await;
                return Err(MediaResponsePublicationRejection::StateChanged);
            }
            if !self.publication_authority_is_current(admitted_serving_generation)
                || session.control.is_retired()
            {
                return Err(MediaResponsePublicationRejection::StateChanged);
            }
            return Ok(MediaResponseAuthorization {
                session_id: session_id.to_owned(),
                owner: owner.clone(),
                release_gate: Arc::clone(&release_gate),
                admitted_serving_generation,
                kind: publication.kind,
                object_name: publication.object_name,
                rolling_generation_metadata_fingerprint,
                scratch_pin: None,
            });
        }

        let MediaResponseOwnerKind::Vod(vod_owner) = &owner.0 else {
            unreachable!("rolling response owner returned above")
        };
        #[cfg(test)]
        let admission_pause = self
            .vod_publication_admission_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        #[cfg(test)]
        if let Some(admission_pause) = admission_pause {
            require_publication_authority!();
            admission_pause.wait().await;
            admission_pause.wait().await;
            require_publication_authority!();
        }
        require_publication_authority!();
        let owner_is_current = match publication.binding {
            MediaResponsePublicationBinding::AttemptStatus => tokio::time::timeout_at(
                tokio::time::Instant::from_std(deadline),
                self.vod
                    .response_status_owner_is_current(session_id, vod_owner),
            )
            .await
            .map_err(|_| MediaResponsePublicationRejection::StateChanged)?,
            MediaResponsePublicationBinding::GenerationMetadata
            | MediaResponsePublicationBinding::AttemptMedia
            | MediaResponsePublicationBinding::ProtocolOnly => tokio::time::timeout_at(
                tokio::time::Instant::from_std(deadline),
                self.vod.response_owner_is_live(session_id, vod_owner),
            )
            .await
            .map_err(|_| MediaResponsePublicationRejection::StateChanged)?,
        };
        require_publication_authority!();
        if !owner_is_current {
            return Err(MediaResponsePublicationRejection::OwnerGone);
        }
        Ok(MediaResponseAuthorization {
            session_id: session_id.to_owned(),
            owner: owner.clone(),
            release_gate,
            admitted_serving_generation,
            kind: publication.kind,
            object_name: publication.object_name,
            rolling_generation_metadata_fingerprint: None,
            scratch_pin: None,
        })
    }

    /// Fence a bodyless startup/failure response against the exact Session
    /// that classified it. A retryable 503 is actor-authorized protocol state;
    /// a final 502 is bound to the exact Session's first-writer failure cell.
    /// A true no-actor 404 has no owner and never enters this path.
    pub(crate) async fn authorize_playlist_error_publication(
        self: &Arc<Self>,
        session_id: &str,
        owner: &MediaResponseOwner,
        error: &PlaylistError,
        deadline: Instant,
    ) -> Result<(), MediaResponsePublicationRejection> {
        if self.session_release_is_active(session_id) {
            return Err(MediaResponsePublicationRejection::StateChanged);
        }
        let admitted_serving_generation = self
            .serving_authority
            .admit()
            .ok_or(MediaResponsePublicationRejection::StateChanged)?;
        macro_rules! require_error_authority {
            () => {
                if !self.publication_authority_is_current(admitted_serving_generation)
                    || self.session_release_is_active(session_id)
                {
                    return Err(MediaResponsePublicationRejection::StateChanged);
                }
            };
        }
        let MediaResponseOwnerKind::Rolling {
            session,
            producer_attempt,
        } = &owner.0
        else {
            return Err(MediaResponsePublicationRejection::OwnerGone);
        };
        require_error_authority!();
        let current = tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            self.sessions.lock(),
        )
        .await
        .map_err(|_| MediaResponsePublicationRejection::StateChanged)?
        .get(session_id)
        .cloned();
        require_error_authority!();
        let exact_current = current
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, session));
        let retired_integrity_owner =
            current.is_none() && session.owns_retired_cache_integrity_failure(error);
        if !exact_current && !retired_integrity_owner {
            return Err(MediaResponsePublicationRejection::OwnerGone);
        }
        let authorized = match error {
            PlaylistError::StartupTimedOut(_) if session.actor_managed_response_publication => {
                let publication = MediaResponsePublication::protocol_only("playlist-error");
                require_error_authority!();
                let authorized = session
                    .control
                    .authorize_response_publication(
                        crate::playback_control::RollingResponsePublication::protocol_only(
                            publication.rolling_object(),
                            *producer_attempt,
                        ),
                        None,
                        deadline,
                    )
                    .await
                    .is_ok();
                require_error_authority!();
                authorized
            }
            PlaylistError::StartupTimedOut(_) => {
                session.control.current_producer_attempt() == *producer_attempt
                    && !session.control.is_retired()
            }
            PlaylistError::ProducerEnded(reason)
                if session.actor_managed_prepublication_process =>
            {
                require_error_authority!();
                let actor_failure = session
                    .published_producer_ended_before(*producer_attempt, deadline)
                    .await;
                require_error_authority!();
                let matches_actor = actor_failure.as_ref().is_some_and(|(actor_error, _)| {
                    matches!(actor_error, PlaylistError::ProducerEnded(actor_reason) if actor_reason == reason)
                });
                if !matches_actor {
                    false
                } else {
                    session
                        .control
                        .authorize_response_publication(
                            crate::playback_control::RollingResponsePublication::attempt_status(
                                // This actor-owned verdict describes the
                                // rendition producer, whether the triggering
                                // request was a playlist reload or a segment
                                // beyond its frontier. ProtocolResponse is not
                                // a valid attempt-status binding.
                                crate::playback_control::RollingResponseObject::VideoMediaPlaylist,
                                *producer_attempt,
                            ),
                            None,
                            deadline,
                        )
                        .await
                        .is_ok()
                }
            }
            PlaylistError::ProducerEnded(_) => false,
            PlaylistError::ProducerExited(_)
            | PlaylistError::SessionFailed(_)
            | PlaylistError::InsufficientCapacity(_) => {
                session.failed.load(Relaxed) && session.failure_reason() == *error
            }
            PlaylistError::SessionGone => false,
        };
        if !authorized {
            return Err(MediaResponsePublicationRejection::StateChanged);
        }
        require_error_authority!();
        let current = tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            self.sessions.lock(),
        )
        .await
        .map_err(|_| MediaResponsePublicationRejection::StateChanged)?
        .get(session_id)
        .cloned();
        require_error_authority!();
        let exact_current = current
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, session));
        let retired_integrity_owner =
            current.is_none() && session.owns_retired_cache_integrity_failure(error);
        if !self.publication_authority_is_current(admitted_serving_generation) {
            Err(MediaResponsePublicationRejection::StateChanged)
        } else if exact_current || retired_integrity_owner {
            Ok(())
        } else {
            Err(MediaResponsePublicationRejection::OwnerGone)
        }
    }

    /// Consume an exact response authorization at advertised EOF. The token
    /// still carries the owner observed before publication, so completion can
    /// never reconstruct authority from a reusable capability string.
    pub(crate) async fn commit_authorized_media(
        self: &Arc<Self>,
        authorization: MediaResponseAuthorization,
        complete_object: bool,
        deadline: Instant,
    ) -> Result<(), MediaResponsePublicationRejection> {
        if tokio::time::Instant::now().into_std() >= deadline {
            return Err(MediaResponsePublicationRejection::StateChanged);
        }
        let _release_transition = tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            Arc::clone(&authorization.release_gate.transition).lock_owned(),
        )
        .await
        .map_err(|_| MediaResponsePublicationRejection::StateChanged)?;
        if authorization.release_gate.released.load(Acquire) {
            return Err(MediaResponsePublicationRejection::StateChanged);
        }
        let _serving_transition = self
            .serving_authority
            .commit_guard_before(authorization.admitted_serving_generation, deadline)
            .await
            .ok_or(MediaResponsePublicationRejection::StateChanged)?;
        if let Some(presentation_contract_fingerprint) = authorization
            .rolling_generation_metadata_fingerprint
            .as_deref()
        {
            let MediaResponseOwnerKind::Rolling { session, .. } = &authorization.owner.0 else {
                return Err(MediaResponsePublicationRejection::StateChanged);
            };
            let current = tokio::time::timeout_at(
                tokio::time::Instant::from_std(deadline),
                self.sessions.lock(),
            )
            .await
            .map_err(|_| MediaResponsePublicationRejection::StateChanged)?
            .get(&authorization.session_id)
            .cloned();
            if !current
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, session))
            {
                return Err(MediaResponsePublicationRejection::OwnerGone);
            }
            if !session
                .control
                .commit_generation_metadata(
                    presentation_contract_fingerprint,
                    authorization.kind,
                    deadline,
                )
                .await
            {
                return Err(MediaResponsePublicationRejection::StateChanged);
            }
            let current = tokio::time::timeout_at(
                tokio::time::Instant::from_std(deadline),
                self.sessions.lock(),
            )
            .await
            .map_err(|_| MediaResponsePublicationRejection::StateChanged)?
            .get(&authorization.session_id)
            .cloned();
            return if current
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, session))
            {
                Ok(())
            } else {
                Err(MediaResponsePublicationRejection::OwnerGone)
            };
        }
        let committed = self
            .commit_resolved_media_before(
                &authorization.session_id,
                &authorization.owner,
                authorization.kind,
                authorization.object_name.as_deref(),
                complete_object,
                deadline,
            )
            .await;
        if committed {
            Ok(())
        } else {
            Err(self
                .response_publication_rejection(
                    &authorization.session_id,
                    &authorization.owner,
                    deadline,
                )
                .await)
        }
    }

    async fn response_publication_rejection(
        &self,
        session_id: &str,
        owner: &MediaResponseOwner,
        deadline: Instant,
    ) -> MediaResponsePublicationRejection {
        match &owner.0 {
            MediaResponseOwnerKind::Rolling {
                session,
                producer_attempt,
            } => {
                let Ok(current) = tokio::time::timeout_at(
                    tokio::time::Instant::from_std(deadline),
                    self.sessions.lock(),
                )
                .await
                else {
                    return MediaResponsePublicationRejection::StateChanged;
                };
                let current = current.get(session_id).cloned();
                if current
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, session))
                    || self
                        .retired_owner_is_current(session_id, session, *producer_attempt)
                        .await
                {
                    MediaResponsePublicationRejection::StateChanged
                } else {
                    MediaResponsePublicationRejection::OwnerGone
                }
            }
            MediaResponseOwnerKind::Vod(owner) => {
                if tokio::time::timeout_at(
                    tokio::time::Instant::from_std(deadline),
                    self.vod.response_owner_is_live(session_id, owner),
                )
                .await
                .unwrap_or(true)
                {
                    MediaResponsePublicationRejection::StateChanged
                } else {
                    MediaResponsePublicationRejection::OwnerGone
                }
            }
        }
    }

    /// Commit a completed response against its resolved incarnation.
    ///
    /// The rolling actor or immutable registry must still own the capability
    /// when response completion linearizes. Partial objects may renew demand,
    /// but only a complete object moves the consumed frontier.
    #[cfg(test)]
    pub(crate) async fn commit_resolved_media(
        self: &Arc<Self>,
        session_id: &str,
        owner: &MediaResponseOwner,
        kind: &'static str,
        object_name: Option<&str>,
        complete_object: bool,
    ) -> bool {
        self.commit_resolved_media_before(
            session_id,
            owner,
            kind,
            object_name,
            complete_object,
            tokio::time::Instant::now().into_std() + Duration::from_secs(5),
        )
        .await
    }

    async fn commit_resolved_media_before(
        self: &Arc<Self>,
        session_id: &str,
        owner: &MediaResponseOwner,
        kind: &'static str,
        object_name: Option<&str>,
        complete_object: bool,
        deadline: Instant,
    ) -> bool {
        if tokio::time::Instant::now().into_std() >= deadline {
            return false;
        }
        if let MediaResponseOwnerKind::Rolling {
            session,
            producer_attempt,
        } = &owner.0
        {
            if self
                .retired_object_is_current(session_id, session, *producer_attempt, object_name)
                .await
            {
                // Read-only grace never renews a lease or advances a retired
                // delivery frontier. Exact owner validation is the complete
                // commit for these already-promised bytes.
                return true;
            }
            let Ok(current) = tokio::time::timeout_at(
                tokio::time::Instant::from_std(deadline),
                self.sessions.lock(),
            )
            .await
            else {
                return false;
            };
            let current = current.get(session_id).cloned();
            if !current
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, session))
            {
                return false;
            }
            let fetched_segment = complete_object
                .then(|| object_name.and_then(segment_index))
                .flatten();
            let fetched_end_ms = if let Some(index) = fetched_segment {
                let Ok(segments) = tokio::time::timeout_at(
                    tokio::time::Instant::from_std(deadline),
                    session.segments.lock(),
                )
                .await
                else {
                    return false;
                };
                segments.end_ms_of(index)
            } else {
                None
            };
            let media_handoff = fetched_segment.map(|index| {
                self.ensure_flow_worker(session_id, Arc::clone(session));
                session.control.media_commit_handoff(
                    *producer_attempt,
                    index,
                    fetched_end_ms,
                    Arc::clone(&session.compatibility_attempt),
                    Arc::clone(&session.high_segment),
                    Arc::clone(&session.fetched_end_ms),
                )
            });
            if !session
                .control
                .commit_media(
                    kind,
                    *producer_attempt,
                    fetched_segment,
                    fetched_end_ms,
                    media_handoff,
                    deadline,
                )
                .await
            {
                return false;
            }
            #[cfg(test)]
            {
                let pause = session
                    .response_projection_pause
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone();
                if let Some(pause) = pause {
                    if tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), async {
                        pause.wait().await;
                        pause.wait().await;
                    })
                    .await
                    .is_err()
                    {
                        return false;
                    }
                }
            }
            return true;
        }
        let vod_index = complete_object
            .then(|| object_name.and_then(segment_index))
            .flatten()
            .and_then(|index| u32::try_from(index).ok());
        let MediaResponseOwnerKind::Vod(owner) = &owner.0 else {
            unreachable!("rolling response owner returned above")
        };
        tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            self.vod.commit_resolved_media(session_id, owner, vod_index),
        )
        .await
        .unwrap_or(false)
    }

    /// Verify response ownership without renewing the lease or moving the
    /// consumed frontier. This is the publication fence for streamed bodies;
    /// their mutating commit happens only after the advertised bytes reach
    /// EOF, so an abandoned response cannot masquerade as client progress.
    #[allow(dead_code)] // Compatibility/test probe; production uses typed authorization.
    pub(crate) async fn response_owner_is_live(
        &self,
        session_id: &str,
        owner: &MediaResponseOwner,
    ) -> bool {
        if let MediaResponseOwnerKind::Rolling {
            session,
            producer_attempt,
        } = &owner.0
        {
            let current = self.sessions.lock().await.get(session_id).cloned();
            return (current
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, session))
                && session.control.current_producer_attempt() == *producer_attempt
                && !session.control.is_retired())
                || self
                    .retired_owner_is_current(session_id, session, *producer_attempt)
                    .await;
        }
        let MediaResponseOwnerKind::Vod(owner) = &owner.0 else {
            unreachable!("rolling response owner returned above")
        };
        self.vod.response_owner_is_live(session_id, owner).await
    }

    /// Resolve frozen presentation facts and the exact current attempt owner
    /// within one HTTP classification deadline. Rolling rows/probe JSON are
    /// never re-read after generation creation. A retired-but-still-registered
    /// Session is a current transition, not proof that the reusable id vanished.
    pub(crate) async fn hls_presentation_before(
        &self,
        session_id: &str,
        deadline: Instant,
    ) -> HlsPresentationResolution {
        if tokio::time::Instant::now().into_std() >= deadline {
            return HlsPresentationResolution::StateChanged;
        }
        let vod_facts = match tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            self.vod.hls_facts(session_id),
        )
        .await
        {
            Ok(facts) => facts,
            Err(_) => return HlsPresentationResolution::StateChanged,
        };
        if let Some(facts) = vod_facts {
            if let Some(encoding) = &facts.encoding {
                let height = encoding.options.target_height;
                let grade = encoding.options.pipeline.output_grade();
                let file = encoded_vod_presentation_file(facts.file, height, grade);
                let mut codecs = transcoded_hls_codecs(grade, height);
                if file.audio_streams.is_empty() {
                    codecs.truncate(codecs.find(',').unwrap_or(codecs.len()));
                }
                return HlsPresentationResolution::Ready(
                    HlsContext {
                        file_id: file.id,
                        start_seconds: 0.0,
                        media_origin_seconds: 0.0,
                        codecs,
                        supplemental_codecs: None,
                        frame_rate: Some(
                            f64::from(encoding.grid.numerator)
                                / f64::from(encoding.grid.denominator),
                        ),
                    },
                    file,
                    MediaResponseOwner(MediaResponseOwnerKind::Vod(facts.response_owner)),
                );
            }
            // The VOD registry freezes the file/recipe but does not yet retain
            // raw codec side data. Preserve its existing Dolby/HEVC contract
            // until that registry grows the missing immutable probe fact;
            // rolling generations below never take this mutable-store path.
            let probe_json = match tokio::time::timeout_at(
                tokio::time::Instant::from_std(deadline),
                self.store.get_file_probe_json(facts.file.id),
            )
            .await
            {
                Ok(result) => result.ok().flatten(),
                Err(_) => return HlsPresentationResolution::StateChanged,
            };
            let (codecs, supplemental_codecs) = copied_hls_codecs(
                &facts.file,
                facts.audio_index,
                CopySessionOptions {
                    transcode_audio: facts.aac,
                    preserve_dolby_vision: facts.preserve_dolby_vision,
                    // A converted stream is described by what it produces, not
                    // by the source's ffprobe record: its sample entry is
                    // `hvc1` and its configuration record says profile 8,
                    // while the source's own record says 7. Passing `false`
                    // here would advertise `dvh1.07.06` for that media.
                    convert_dolby_vision: facts.convert_dolby_vision,
                },
                probe_json.as_deref(),
            );
            return HlsPresentationResolution::Ready(
                HlsContext {
                    file_id: facts.file.id,
                    start_seconds: 0.0,
                    media_origin_seconds: 0.0,
                    codecs,
                    supplemental_codecs,
                    frame_rate: frozen_video_frame_rate(probe_json.as_deref()),
                },
                facts.file,
                MediaResponseOwner(MediaResponseOwnerKind::Vod(facts.response_owner)),
            );
        }
        let session = match tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            self.sessions.lock(),
        )
        .await
        {
            Ok(sessions) => sessions.get(session_id).cloned(),
            Err(_) => return HlsPresentationResolution::StateChanged,
        };
        let Some(session) = session else {
            return HlsPresentationResolution::Gone;
        };
        if session.failed.load(Relaxed) {
            return HlsPresentationResolution::Failed(PlaylistPublicationError::for_session(
                session.failure_reason(),
                &session,
            ));
        }
        if session.control.is_retired() {
            return HlsPresentationResolution::StateChanged;
        }
        let Some(presentation) = session.frozen_presentation.as_ref().cloned() else {
            return HlsPresentationResolution::StateChanged;
        };
        HlsPresentationResolution::Ready(
            presentation.context,
            presentation.file,
            MediaResponseOwner(MediaResponseOwnerKind::Rolling {
                producer_attempt: session.control.current_producer_attempt(),
                session,
            }),
        )
    }

    /// Resolve immutable presentation facts through an already-fenced
    /// response owner. Rolling failures may retire their actor while the
    /// exact Session remains registered; frozen subtitle metadata must not
    /// turn that typed state into a live-facade 404 before final publication
    /// admission gets to revalidate the owner.
    pub(crate) async fn hls_presentation_for_owner_before(
        &self,
        session_id: &str,
        owner: &MediaResponseOwner,
        deadline: Instant,
    ) -> Result<(HlsContext, plurx_core::domain::MediaFile), MediaResponsePublicationRejection>
    {
        if tokio::time::Instant::now().into_std() >= deadline {
            return Err(MediaResponsePublicationRejection::StateChanged);
        }
        if let MediaResponseOwnerKind::Rolling {
            session,
            producer_attempt,
        } = &owner.0
        {
            let current = tokio::time::timeout_at(
                tokio::time::Instant::from_std(deadline),
                self.sessions.lock(),
            )
            .await
            .map_err(|_| MediaResponsePublicationRejection::StateChanged)?
            .get(session_id)
            .cloned();
            let exact_live = current
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, session));
            let exact_retired = self
                .retired_owner_is_current(session_id, session, *producer_attempt)
                .await;
            if !exact_live && !exact_retired {
                return Err(MediaResponsePublicationRejection::OwnerGone);
            }
            if !exact_retired
                && (session.failed.load(Relaxed)
                    || session.control.is_retired()
                    || session.control.current_producer_attempt() != *producer_attempt)
            {
                return Err(MediaResponsePublicationRejection::StateChanged);
            }
            let presentation = session
                .frozen_presentation
                .as_ref()
                .cloned()
                .ok_or(MediaResponsePublicationRejection::StateChanged)?;
            return Ok((presentation.context, presentation.file));
        }
        let MediaResponseOwnerKind::Vod(vod_owner) = &owner.0 else {
            unreachable!("rolling response owner returned above")
        };
        if !tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            self.vod.response_owner_is_live(session_id, vod_owner),
        )
        .await
        .map_err(|_| MediaResponsePublicationRejection::StateChanged)?
        {
            return Err(MediaResponsePublicationRejection::OwnerGone);
        }
        match self.hls_presentation_before(session_id, deadline).await {
            HlsPresentationResolution::Ready(context, file, _) => Ok((context, file)),
            HlsPresentationResolution::Gone => Err(MediaResponsePublicationRejection::OwnerGone),
            HlsPresentationResolution::Failed(_) | HlsPresentationResolution::StateChanged => {
                Err(MediaResponsePublicationRejection::StateChanged)
            }
        }
    }

    /// Read the current media playlist for a session.
    ///
    /// Every terminal cause is named rather than collapsed — see
    /// [`PlaylistError`] for why an anonymous `None` was not survivable
    /// downstream.
    #[cfg(test)]
    pub async fn playlist(self: &Arc<Self>, session_id: &str) -> Result<Vec<u8>, PlaylistError> {
        self.playlist_with_owner(session_id)
            .await
            .map(|(bytes, _)| bytes)
            .map_err(|error| error.error)
    }

    /// Resolve playlist bytes together with the exact rolling attempt that
    /// supplied them. Composite resources such as native subtitle playlists
    /// must carry this owner through their own response commit rather than
    /// reconstructing ownership from a reusable session id afterward.
    #[cfg(test)]
    pub(crate) async fn playlist_with_owner(
        self: &Arc<Self>,
        session_id: &str,
    ) -> Result<(Vec<u8>, MediaResponseOwner), PlaylistPublicationError> {
        self.playlist_with_owner_before(session_id, self.playlist_request_deadline())
            .await
    }

    /// One absolute HTTP patience boundary. Reclassification may inspect a
    /// newer attempt, but it must never mint another complete startup budget.
    pub(crate) fn playlist_request_deadline(&self) -> Instant {
        tokio::time::Instant::now().into_std() + self.playlist_wait()
    }

    pub(crate) async fn playlist_with_owner_before(
        self: &Arc<Self>,
        session_id: &str,
        deadline: Instant,
    ) -> Result<(Vec<u8>, MediaResponseOwner), PlaylistPublicationError> {
        let budget = self.playlist_wait();
        if tokio::time::Instant::now().into_std() >= deadline {
            return Err(PlaylistPublicationError::without_owner(
                PlaylistError::StartupTimedOut(budget),
            ));
        }
        // Resolve the exact registry incarnation even if its actor has just
        // retired. A prepublication failure is stored before End and must
        // remain publishable as its typed 502 while that exact Session still
        // occupies the id; prefiltering retired actors here would erase the
        // failure into an anonymous 404 before the owner fence can validate it.
        let session = tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            self.sessions.lock(),
        )
        .await
        .ok()
        .and_then(|sessions| sessions.get(session_id).cloned());
        let Some(session) = session else {
            if tokio::time::Instant::now().into_std() >= deadline {
                return Err(PlaylistPublicationError::without_owner(
                    PlaylistError::StartupTimedOut(budget),
                ));
            }
            return Err(PlaylistPublicationError::gone());
        };
        session.ensure_publication_worker(session_id);
        match tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            self.playlist_with_owner_for_session_before(
                session_id,
                Arc::clone(&session),
                deadline,
                budget,
            ),
        )
        .await
        {
            Ok(result) => result,
            Err(_) if session.failed.load(Relaxed) => Err(PlaylistPublicationError::for_session(
                session.failure_reason(),
                &session,
            )),
            Err(_) if session.control.is_retired() => Err(PlaylistPublicationError::for_session(
                PlaylistError::StartupTimedOut(budget),
                &session,
            )),
            Err(_) => {
                tracing::warn!(
                    session = %session_log_id(session_id),
                    waited_s = budget.as_secs(),
                    "playlist preparation exhausted its one absolute HTTP budget"
                );
                Err(PlaylistPublicationError::for_session(
                    PlaylistError::StartupTimedOut(budget),
                    &session,
                ))
            }
        }
    }

    pub(super) async fn playlist_with_owner_for_session_before(
        self: &Arc<Self>,
        session_id: &str,
        session: Arc<Session>,
        deadline: Instant,
        budget: Duration,
    ) -> Result<(Vec<u8>, MediaResponseOwner), PlaylistPublicationError> {
        // Hold the request until the playlist exists. On the transcode path
        // that is a beat after ffmpeg starts; on the copy path it is the
        // publish gate filling (COPY_PUBLISH_GATE_SECS), which on a
        // NAS-bound 4K remux is production time in the double-digit seconds;
        // and on a stalled hardware start it is the whole
        // hardware→software recovery, which is why the window is
        // PLAYLIST_WAIT_BUDGET rather than a number chosen here. Holding
        // beats erroring because the client's patience is asymmetric,
        // verified against the vendored hls.js: a slow first byte is waited
        // on indefinitely (manifestLoadPolicy.maxTimeToFirstByteMs: Infinity,
        // 20 s per attempt, two timeout retries), while an error response
        // spends one of a single error retry. A failed session still returns
        // immediately — the budget below is only ever spent on a session that
        // is genuinely still starting, never on one that has already lost.
        loop {
            // `touch` resolves the Arc once, but retirement is allowed to
            // remove that session while this request is waiting. The Arc
            // keeps the scratch state alive; it does not keep the session
            // addressable. Re-check the monotonic retirement verdict before
            // reading or serving so a superseded stream is reported as gone
            // promptly rather than held until the startup budget expires.
            if session.failed.load(Relaxed) {
                return Err(PlaylistPublicationError::for_session(
                    session.failure_reason(),
                    &session,
                ));
            }
            if session.control.is_retired() {
                return Err(PlaylistPublicationError::for_session(
                    PlaylistError::StartupTimedOut(budget),
                    &session,
                ));
            }
            let Some(producer_attempt) = session.coherent_path_producer_attempt().await else {
                if tokio::time::Instant::now().into_std() >= deadline {
                    return Err(PlaylistPublicationError::for_session(
                        PlaylistError::StartupTimedOut(budget),
                        &session,
                    ));
                }
                wait_for_playlist_poll_before(deadline).await;
                continue;
            };
            // Every rejected snapshot rejoins the same terminal/deadline/poll
            // path. The outer HTTP timeout bounds blocked I/O; this loop owns
            // retry pacing even when storage and actor replies are immediate.
            'snapshot: {
                // A startup request is allowed to span a pre-publication
                // fallback. Fence each concrete read, not the whole wait, so old
                // bytes are retried while valid successor bytes keep the original
                // request alive.
                let playlist_bytes = if let Some(manifest) = &session.cache_manifest {
                    if session
                        .cache_location
                        .as_ref()
                        .is_some_and(|location| location.storage_class == "shared")
                    {
                        let Some(shared_cache) = self.shared_cache.as_ref() else {
                            return Err(PlaylistPublicationError::for_session(
                                PlaylistError::SessionFailed(
                                    "shared cache coordinator is unavailable".to_owned(),
                                ),
                                &session,
                            ));
                        };
                        let manifest = Arc::clone(manifest);
                        let directory = session.dir.clone();
                        shared_cache
                            .run_mount_io("shared_playlist_read_timeout", async move {
                                manifest
                                    .read_verified_playlist(&directory, "index.m3u8")
                                    .await
                                    .map_err(|error| error.to_string())
                            })
                            .await
                            .ok()
                            .flatten()
                    } else {
                        manifest
                            .read_verified_playlist(&session.dir, "index.m3u8")
                            .await
                            .ok()
                            .flatten()
                    }
                } else if session.cached {
                    plurx_core::transcode::manifest::read_bounded_playlist(
                        &session.dir,
                        "index.m3u8",
                    )
                    .await
                    .ok()
                    .flatten()
                } else {
                    session
                        .served_playlist(producer_attempt)
                        .await
                        .map(|bytes| bytes.as_ref().to_vec())
                };
                if session.replacing_child.load(Acquire)
                    || session.control.current_producer_attempt() != producer_attempt
                    || session.compatibility_producer_attempt() != producer_attempt
                {
                    break 'snapshot;
                }
                if let Some(bytes) = playlist_bytes {
                    if !bytes.is_empty() {
                        // A response body admitted before the actor's failure
                        // verdict owns its bytes through EOF. A later playlist
                        // reload is new demand beyond the retained frontier and
                        // must receive the typed terminal producer state rather
                        // than replay this partial EVENT playlist forever.
                        if let Some((error, _)) = session
                            .published_producer_ended_before(producer_attempt, deadline)
                            .await
                        {
                            return Err(PlaylistPublicationError::for_session(error, &session));
                        }
                        if !session.cached {
                            if let Err(reason) = validate_rolling_target(&bytes) {
                                tracing::error!(
                                    session = %session_log_id(session_id),
                                    producer_attempt,
                                    %reason,
                                    "rolling writer violated its fixed presentation target"
                                );
                                session.fail(PlaylistError::SessionFailed(reason));
                                let (_commit, _retirement) = spawn_rolling_retirement_owner(
                                    Arc::clone(&self.sessions),
                                    Arc::clone(&self.retired_presentations),
                                    Arc::clone(&self.active_session_count),
                                    Arc::clone(&self.store),
                                    Arc::clone(&self.recent_marker_ambiguities),
                                    session_id.to_owned(),
                                    Arc::clone(&session),
                                    None,
                                    "playlist_contract",
                                );
                                return Err(PlaylistPublicationError::for_session(
                                    session.failure_reason(),
                                    &session,
                                ));
                            }
                        }
                        // ffmpeg rewrites an EVENT playlist after each segment. Do
                        // not let hls.js race away with the first one-segment
                        // version: its first reload is scheduled at the exact edge
                        // of that segment, which leaves no time for request + append
                        // and creates a deterministic startup stall. This is only a
                        // first-response gate; cached VOD and copy have already
                        // opened it in their constructors, and every reload after
                        // the publication store below stays on the old fast path.
                        let Some(playlist_published) =
                            session.compatibility_playlist_published(producer_attempt)
                        else {
                            break 'snapshot;
                        };
                        if !playlist_published && !transcode_first_playlist_ready(&bytes) {
                            break 'snapshot;
                        }
                        // Actor readiness must come from the exact bytes this
                        // response owns. A best-effort second filesystem read in
                        // flow control cannot be the publication linearization
                        // point: it may fail after these bytes were validated.
                        let exact_index = SegmentIndex {
                            segs: parse_playlist(&String::from_utf8_lossy(&bytes)),
                            revision: 0,
                        };
                        #[cfg(test)]
                        session.pause_playlist_publication_for_test().await;
                        if !session
                            .control
                            .observe_publication_before(
                                crate::playback_control::RollingPublicationObservation {
                                    producer_attempt,
                                    publication_commit: false,
                                    demand_sequence: None,
                                    produced_segment: exact_index
                                        .segs
                                        .last()
                                        .map(|segment| segment.index),
                                    produced_end_ms: exact_index.produced_playable_end_ms(),
                                    playlist_ready: true,
                                    published_segment: exact_index
                                        .segs
                                        .last()
                                        .map(|segment| segment.index),
                                    published_end_ms: exact_index.produced_playable_end_ms(),
                                    published_first_segment: None,
                                    published_start_ms: None,
                                    media_origin_ms: (session.media_origin_seconds * 1_000.0)
                                        .round()
                                        as i64,
                                    next_media_sequence: exact_index.next_media_sequence(),
                                    resolved_fetched_segment: None,
                                    resolved_fetched_end_ms: None,
                                },
                                deadline,
                            )
                            .await
                        {
                            break 'snapshot;
                        }
                        // The exact bytes above are the response prerequisite.
                        // Refreshing storage/accounting and signalling the child
                        // is consequential background work, so queue it on the
                        // session-owned worker instead of extending (or inheriting
                        // cancellation from) this HTTP request.
                        self.ensure_flow_worker(session_id, Arc::clone(&session));
                        session.control.request_flow();
                        if session.control.is_retired() {
                            if session.failed.load(Relaxed) {
                                return Err(PlaylistPublicationError::for_session(
                                    session.failure_reason(),
                                    &session,
                                ));
                            }
                            return Err(PlaylistPublicationError::for_session(
                                PlaylistError::StartupTimedOut(budget),
                                &session,
                            ));
                        }
                        if session.replacing_child.load(Acquire)
                            || session.control.current_producer_attempt() != producer_attempt
                            || session.compatibility_producer_attempt() != producer_attempt
                        {
                            break 'snapshot;
                        }
                        if session.cached {
                            return Ok((
                                bytes,
                                MediaResponseOwner(MediaResponseOwnerKind::Rolling {
                                    session: Arc::clone(&session),
                                    producer_attempt,
                                }),
                            ));
                        }
                        let first_retained = session.segments.lock().await.first_retained_index();
                        // A successor begins numbering at its epoch floor, so
                        // "has anything been pruned" is measured from that floor
                        // and a fresh takeover does not report itself as sliding.
                        let slide_baseline = session
                            .takeover
                            .as_ref()
                            .map_or(0, |takeover| takeover.media_sequence);
                        if let Some(first_retained_index) =
                            first_retained.filter(|index| *index > slide_baseline)
                        {
                            if !session.first_slide_logged.swap(true, Relaxed) {
                                let now_unix = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|duration| duration.as_secs() as i64)
                                    .unwrap_or(session.started_unix);
                                tracing::info!(
                                    session = %session_log_id(session_id),
                                    first_retained_index,
                                    wall_seconds_since_start =
                                        now_unix.saturating_sub(session.started_unix),
                                    "served HLS playlist began sliding"
                                );
                                let manager = Arc::clone(self);
                                let event_session = Arc::clone(&session);
                                let event_session_id = session_id.to_owned();
                                tokio::spawn(async move {
                                    manager
                                        .emit_session_event(
                                            &event_session_id,
                                            &event_session,
                                            "playlist_slide",
                                            SessionEventFields {
                                                extra: Some(
                                                    serde_json::json!({
                                                        "first_retained_index": first_retained_index
                                                    })
                                                    .to_string(),
                                                ),
                                                ..SessionEventFields::default()
                                            },
                                        )
                                        .await;
                                });
                            }
                        }
                        if session.control.is_retired() {
                            if session.failed.load(Relaxed) {
                                return Err(PlaylistPublicationError::for_session(
                                    session.failure_reason(),
                                    &session,
                                ));
                            }
                            return Err(PlaylistPublicationError::for_session(
                                PlaylistError::StartupTimedOut(budget),
                                &session,
                            ));
                        }
                        return Ok((
                            bytes,
                            MediaResponseOwner(MediaResponseOwnerKind::Rolling {
                                session: Arc::clone(&session),
                                producer_attempt,
                            }),
                        ));
                    }
                }
                if session.cached {
                    tracing::error!(
                        session = %session_log_id(session_id),
                        "cached playlist was missing, empty, oversized, or failed its manifest"
                    );
                    self.fail_cached_session_integrity(
                        session_id,
                        &session,
                        "playlist_object_mismatch",
                    );
                    return Err(PlaylistPublicationError::for_session(
                        session.failure_reason(),
                        &session,
                    ));
                }
            }
            if let Some((error, _)) = session
                .published_producer_ended_before(producer_attempt, deadline)
                .await
            {
                return Err(PlaylistPublicationError::for_session(error, &session));
            }
            if session.failed.load(Relaxed) {
                return Err(PlaylistPublicationError::for_session(
                    session.failure_reason(),
                    &session,
                ));
            }
            if session.control.is_retired() {
                return Err(PlaylistPublicationError::for_session(
                    PlaylistError::StartupTimedOut(budget),
                    &session,
                ));
            }
            // Checked after the terminal verdicts, never before them: a
            // session that has already lost must not be reported as one that
            // merely ran out of time, and a session that is still inside the
            // recovery path must not be reported as terminal.
            if tokio::time::Instant::now().into_std() >= deadline {
                tracing::warn!(
                    session = %session_log_id(session_id),
                    waited_s = budget.as_secs(),
                    "no usable HLS playlist within the startup budget; telling the client \
                     to retry rather than that the stream failed"
                );
                return Err(PlaylistPublicationError::for_session(
                    PlaylistError::StartupTimedOut(budget),
                    &session,
                ));
            }
            wait_for_playlist_poll_before(deadline).await;
        }
    }

    /// How long one playlist request holds a still-starting session.
    ///
    /// Production always answers [`PLAYLIST_WAIT_BUDGET`]. The override exists
    /// so the deadline's *mechanics* can be exercised in milliseconds; the
    /// budget's *value* is pinned separately against the recovery graces it
    /// has to outlive, which is the relation whose absence caused #263.
    pub(super) fn playlist_wait(&self) -> Duration {
        {
            let ms = self.playlist_wait_override_ms.load(Relaxed);
            if ms > 0 {
                return Duration::from_millis(ms);
            }
        }
        PLAYLIST_WAIT_BUDGET
    }

    /// Resolve one segment against the complete session timeline.
    ///
    /// The served media playlist becomes a sliding window after retention
    /// starts, but subtitle cues still need the elapsed duration of the
    /// pruned prefix. The index keeps those duration-only entries even after
    /// their files are gone, so callers never reconstruct time from a segment
    /// number or the shortened playlist.
    #[cfg(test)]
    pub async fn segment_window(&self, session_id: &str, segment_index: i64) -> Option<(f64, f64)> {
        if let Some(window) = self.vod.segment_window(session_id, segment_index).await {
            return Some(window);
        }
        let session = self.live_session(session_id).await?;
        self.flow_control(&session, session_id).await;
        let (start_ms, end_ms) = session.segments.lock().await.window_ms_of(segment_index)?;
        Some((start_ms as f64 / 1000.0, end_ms as f64 / 1000.0))
    }

    /// Resolve subtitle timing through the same exact owner as the VTT body.
    /// The old live-only facade erased terminal/transition state before HTTP
    /// could classify it and could read a successor's catalog after an ABA.
    pub(crate) async fn segment_window_for_owner_before(
        &self,
        session_id: &str,
        segment_index: i64,
        owner: &MediaResponseOwner,
        deadline: Instant,
    ) -> Result<Option<(f64, f64)>, MediaResponsePublicationRejection> {
        if tokio::time::Instant::now().into_std() >= deadline {
            return Err(MediaResponsePublicationRejection::StateChanged);
        }
        if let MediaResponseOwnerKind::Rolling {
            session,
            producer_attempt,
        } = &owner.0
        {
            let current = tokio::time::timeout_at(
                tokio::time::Instant::from_std(deadline),
                self.sessions.lock(),
            )
            .await
            .map_err(|_| MediaResponsePublicationRejection::StateChanged)?
            .get(session_id)
            .cloned();
            let exact_live = current
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, session));
            let exact_retired = self
                .retired_owner_is_current(session_id, session, *producer_attempt)
                .await;
            if !exact_live && !exact_retired {
                return Err(MediaResponsePublicationRejection::OwnerGone);
            }
            if !exact_retired
                && (session.failed.load(Relaxed)
                    || session.control.is_retired()
                    || session.control.current_producer_attempt() != *producer_attempt)
            {
                return Err(MediaResponsePublicationRejection::StateChanged);
            }
            let window = tokio::time::timeout_at(
                tokio::time::Instant::from_std(deadline),
                session.segments.lock(),
            )
            .await
            .map_err(|_| MediaResponsePublicationRejection::StateChanged)?
            .window_ms_of(segment_index)
            .map(|(start_ms, end_ms)| (start_ms as f64 / 1000.0, end_ms as f64 / 1000.0));
            return Ok(window);
        }
        let MediaResponseOwnerKind::Vod(vod_owner) = &owner.0 else {
            unreachable!("rolling response owner returned above")
        };
        if !tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            self.vod.response_owner_is_live(session_id, vod_owner),
        )
        .await
        .map_err(|_| MediaResponsePublicationRejection::StateChanged)?
        {
            return Err(MediaResponsePublicationRejection::OwnerGone);
        }
        tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            self.vod.segment_window(session_id, segment_index),
        )
        .await
        .map_err(|_| MediaResponsePublicationRejection::StateChanged)
    }

    /// Open a segment for streaming, waiting for ffmpeg to produce it if
    /// necessary.
    ///
    /// Returns an open handle rather than the bytes. One segment of 4K
    /// copy is around 35 MB, and reading that into a `Vec` before Axum sends
    /// its first byte is 35 MB of allocation and a memcpy per request, per
    /// session, four times a minute — for data that is about to be copied
    /// straight back out to a socket. Handing over the open file lets the
    /// response stream, and opening it *here* closes the window where the
    /// retention sweep could unlink the path between resolving it and reading
    /// it: an unlinked file that is already open stays readable.
    pub(crate) async fn segment_for_publication(
        self: &Arc<Self>,
        session_id: &str,
        name: &str,
    ) -> Result<SegmentPublication, SegmentOpenError> {
        self.segment_for_publication_before(session_id, name, Instant::now() + SEGMENT_WAIT)
            .await
    }

    pub(crate) async fn segment_for_publication_before(
        self: &Arc<Self>,
        session_id: &str,
        name: &str,
        deadline: Instant,
    ) -> Result<SegmentPublication, SegmentOpenError> {
        // Guard against path traversal: segment names are `segNNNNN.ts` only.
        if !is_safe_segment(name) {
            return Ok(SegmentPublication::Missing(None));
        }
        let live_session = tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            self.sessions.lock(),
        )
        .await
        .map_err(|_| SegmentOpenError::Capacity)?
        .get(session_id)
        .cloned();
        let retired = if live_session.is_none() {
            tokio::time::timeout_at(
                tokio::time::Instant::from_std(deadline),
                self.retired_presentations.lock(),
            )
            .await
            .map_err(|_| SegmentOpenError::Capacity)?
            .get(session_id)
            .filter(|retired| Instant::now() < retired.serve_until)
            .cloned()
        } else {
            None
        };
        let (session, retired_owner, resolved_attempt) = match (live_session, retired) {
            (Some(session), _) => {
                let attempt = session.control.current_producer_attempt();
                (session, false, attempt)
            }
            (None, Some(retired)) => (retired.session, true, retired.producer_attempt),
            (None, None) => return Ok(SegmentPublication::Missing(None)),
        };
        // Negative classifications must carry the attempt that was current
        // when this lookup began. If fallback crosses any later await, HTTP's
        // AttemptStatus admission rejects this owner instead of authorizing a
        // predecessor-derived 404 against the successor attempt.
        let current_owner = || {
            MediaResponseOwner(MediaResponseOwnerKind::Rolling {
                session: Arc::clone(&session),
                producer_attempt: resolved_attempt,
            })
        };
        if !retired_owner && session.failed.load(Relaxed) {
            return Ok(SegmentPublication::Failed(
                PlaylistPublicationError::for_session(session.failure_reason(), &session),
            ));
        }
        if !retired_owner && session.control.is_retired() {
            return Ok(SegmentPublication::Pending(current_owner()));
        }
        let path = session.dir.join(name);
        let idx = segment_index(name);
        let (first_retained, requested_visibility) = {
            let segments = session.segments.lock().await;
            (
                segments.first_retained_index(),
                idx.and_then(|index| {
                    segments
                        .segs
                        .iter()
                        .find(|segment| segment.index == index)
                        .map(|segment| segment.visibility)
                }),
            )
        };
        if requested_visibility.is_some_and(|visibility| !visibility.is_servable(Instant::now())) {
            tracing::warn!(
                session = %session_log_id(session_id),
                segment = name,
                first_retained_segment = ?first_retained,
                "HLS segment request fell behind the retained playlist window"
            );
            self.emit_session_event(
                session_id,
                &session,
                "segment_unavailable",
                SessionEventFields {
                    reason: Some("segment_pruned"),
                    ms: Some(0),
                    extra: Some(
                        serde_json::json!({
                            "segment": name,
                            "requested_segment": idx,
                            "first_retained_segment": first_retained
                        })
                        .to_string(),
                    ),
                    ..SessionEventFields::default()
                },
            )
            .await;
            return Ok(SegmentPublication::Missing(Some(current_owner())));
        }

        let mut authenticated_cached_file = if let Some(manifest) = &session.cache_manifest {
            // A valid HLS-shaped name is not proof that this generation ever
            // published it. Client probes and stale playlist requests are an
            // ordinary miss; only a listed object whose bytes fail validation
            // can convict and retire the generation.
            if !manifest.contains_object(name) {
                return Ok(SegmentPublication::Missing(Some(current_owner())));
            }
            let verified_object = if session
                .cache_location
                .as_ref()
                .is_some_and(|location| location.storage_class == "shared")
            {
                let Some(shared_cache) = self.shared_cache.as_ref() else {
                    return Err(SegmentOpenError::Capacity);
                };
                let manifest = Arc::clone(manifest);
                let directory = session.dir.clone();
                let name = name.to_owned();
                match shared_cache
                    .run_mount_io("shared_segment_read_timeout", async move {
                        Ok(manifest.open_verified_object(&directory, &name).await)
                    })
                    .await
                {
                    Ok(result) => result,
                    Err(_) => return Err(SegmentOpenError::Capacity),
                }
            } else {
                manifest.open_verified_object(&session.dir, name).await
            };
            match verified_object {
                Ok(Some(opened)) => Some(opened),
                Err(error) if error.is_capacity() => return Err(SegmentOpenError::Capacity),
                Ok(None) | Err(_) => {
                    tracing::error!(
                        session = %session_log_id(session_id),
                        segment = name,
                        "cached object failed its generation manifest"
                    );
                    // Ownership transfer is deliberately before telemetry:
                    // the request can be cancelled at any later await without
                    // abandoning invalidation or exact-session retirement.
                    self.fail_cached_session_integrity(
                        session_id,
                        &session,
                        "segment_object_mismatch",
                    );
                    self.emit_session_event(
                        session_id,
                        &session,
                        "segment_unavailable",
                        SessionEventFields {
                            reason: Some("cache_integrity"),
                            extra: Some(serde_json::json!({"segment": name}).to_string()),
                            ..SessionEventFields::default()
                        },
                    )
                    .await;
                    return Ok(SegmentPublication::Failed(
                        PlaylistPublicationError::for_session(session.failure_reason(), &session),
                    ));
                }
            }
        } else {
            None
        };

        let started_waiting = Instant::now();
        // Taken on the second pass, not here: a segment that is already
        // published is served on the first pass without ever counting as a
        // wait. Every later pass follows a sleep or a replacement retry, so
        // one site covers every way this loop can wait — including the
        // final sleep, the only one a cached session reaches.
        let mut parked: Option<HttpWaitGuard> = None;
        let mut first_pass = true;
        loop {
            if !std::mem::take(&mut first_pass) {
                parked.get_or_insert_with(|| HttpWaitGuard::enter(&session, idx));
            }
            let producer_attempt = if retired_owner {
                Some(resolved_attempt)
            } else {
                session.coherent_path_producer_attempt().await
            };
            let Some(producer_attempt) = producer_attempt else {
                if Instant::now() >= deadline {
                    if session.failed.load(Relaxed) {
                        return Ok(SegmentPublication::Failed(
                            PlaylistPublicationError::for_session(
                                session.failure_reason(),
                                &session,
                            ),
                        ));
                    }
                    return Ok(SegmentPublication::Pending(current_owner()));
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            };
            if !session.cached {
                let current_visibility = if let Some(index) = idx {
                    session
                        .segments
                        .lock()
                        .await
                        .segs
                        .iter()
                        .find(|segment| segment.index == index)
                        .map(|segment| segment.visibility)
                } else {
                    None
                };
                if current_visibility
                    .is_some_and(|visibility| !visibility.is_servable(Instant::now()))
                {
                    return Ok(SegmentPublication::Missing(Some(current_owner())));
                }
                let available = if retired_owner {
                    let owner_current = self
                        .retired_presentations
                        .lock()
                        .await
                        .get(session_id)
                        .is_some_and(|retired| {
                            retired.producer_attempt == producer_attempt
                                && Arc::ptr_eq(&retired.session, &session)
                                && Instant::now() < retired.serve_until
                        });
                    owner_current
                        && idx.is_none_or(|_| {
                            current_visibility
                                .is_some_and(|visibility| visibility.is_servable(Instant::now()))
                        })
                } else {
                    session
                        .publication
                        .lock()
                        .await
                        .served
                        .as_ref()
                        .filter(|snapshot| snapshot.producer_attempt == producer_attempt)
                        .is_some_and(|snapshot| {
                            idx.is_none_or(|index| {
                                current_visibility.is_some_and(|visibility| {
                                    visibility.is_servable(Instant::now())
                                }) || (snapshot.first_segment <= index
                                    && index <= snapshot.last_segment)
                            })
                        })
                };
                if !available {
                    if retired_owner {
                        return Ok(SegmentPublication::Missing(Some(current_owner())));
                    }
                    if Instant::now() >= deadline {
                        return Ok(SegmentPublication::Pending(current_owner()));
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            }
            let opened = if let Some(opened) = authenticated_cached_file.take() {
                Some((opened.file, Some(opened.bytes), Some(opened.lease)))
            } else if session.cached {
                plurx_core::transcode::manifest::open_bounded_regular_object(&session.dir, name)
                    .await
                    .ok()
                    .flatten()
                    .map(|(file, len)| (file, Some(len), None))
            } else {
                tokio::fs::File::open(&path)
                    .await
                    .ok()
                    .map(|file| (file, None, None))
            };
            if let Some((file, authenticated_len, snapshot_lease)) = opened {
                let len = match authenticated_len {
                    Some(len) => len,
                    None => match file.metadata().await {
                        Ok(metadata) => metadata.len(),
                        Err(_) => {
                            return Ok(SegmentPublication::Unavailable(MediaResponseOwner(
                                MediaResponseOwnerKind::Rolling {
                                    session: Arc::clone(&session),
                                    producer_attempt,
                                },
                            )));
                        }
                    },
                };
                // The handle and its owner fence must describe the same
                // producer attempt. If replacement crossed the open, discard
                // the old handle and retry against the successor directory;
                // sampling only at EOF would let predecessor bytes advance
                // the successor's frontier.
                if retired_owner {
                    let exact = self
                        .retired_presentations
                        .lock()
                        .await
                        .get(session_id)
                        .is_some_and(|retired| {
                            retired.producer_attempt == producer_attempt
                                && Arc::ptr_eq(&retired.session, &session)
                                && Instant::now() < retired.serve_until
                        });
                    if !exact {
                        return Ok(SegmentPublication::Missing(Some(current_owner())));
                    }
                } else if session.replacing_child.load(Acquire)
                    || session.control.current_producer_attempt() != producer_attempt
                    || session.compatibility_producer_attempt() != producer_attempt
                {
                    continue;
                }
                let waited = started_waiting.elapsed();
                if idx.is_some() && waited >= SEGMENT_WAIT_EVENT_MIN {
                    let waited_ms = waited.as_millis().min(i64::MAX as u128) as i64;
                    tracing::warn!(
                        session = %session_log_id(session_id),
                        segment = name,
                        waited_ms,
                        progress_idle_ms = session
                            .progress
                            .stalled_for()
                            .as_millis()
                            .min(i64::MAX as u128) as i64,
                        "HLS segment became available after the client had to wait"
                    );
                    self.emit_session_event(
                        session_id,
                        &session,
                        "segment_wait",
                        SessionEventFields {
                            reason: Some("producer_late"),
                            ms: Some(waited_ms),
                            extra: Some(
                                serde_json::json!({
                                    "segment": name,
                                    "progress_idle_ms": session
                                        .progress
                                        .stalled_for()
                                        .as_millis()
                                        .min(i64::MAX as u128) as i64
                                })
                                .to_string(),
                            ),
                            ..SessionEventFields::default()
                        },
                    )
                    .await;
                }
                let encoder = (*session.encoder_label.lock().await).to_owned();
                let (segment_start_ms, segment_duration_ms) = match idx {
                    Some(index) => session.segments.lock().await.window_ms_of(index).map_or(
                        (None, None),
                        |(start_ms, end_ms)| {
                            (Some(start_ms), Some(end_ms.saturating_sub(start_ms)))
                        },
                    ),
                    None => (None, None),
                };
                let delivery = SegmentDelivery::new(
                    SegmentDeliveryContext {
                        store: Arc::clone(&self.store),
                        session: Arc::clone(&session),
                        producer_attempt,
                        session_id: session_id.to_owned(),
                        segment: name.to_owned(),
                        segment_start_ms,
                        segment_duration_ms,
                        encoder,
                    },
                    len,
                    snapshot_lease,
                );
                return Ok(SegmentPublication::Ready(Box::new(SegmentFile {
                    file,
                    len,
                    delivery,
                })));
            }
            if retired_owner {
                return Ok(SegmentPublication::Missing(Some(current_owner())));
            }
            // Give up if the session was declared dead, or ffmpeg has exited and
            // the file still isn't there.
            if session.failed.load(Relaxed) {
                let failure = session.failure_reason();
                let waited_ms = started_waiting.elapsed().as_millis().min(i64::MAX as u128) as i64;
                tracing::error!(
                    session = %session_log_id(session_id),
                    segment = name,
                    waited_ms,
                    reason = failure.code(),
                    "HLS segment request ended because its producer failed"
                );
                self.emit_session_event(
                    session_id,
                    &session,
                    "segment_unavailable",
                    SessionEventFields {
                        reason: Some(failure.code()),
                        ms: Some(waited_ms),
                        extra: Some(
                            serde_json::json!({
                                "segment": name,
                                "failure": failure.message(),
                                "progress_idle_ms": session
                                    .progress
                                    .stalled_for()
                                    .as_millis()
                                    .min(i64::MAX as u128) as i64
                            })
                            .to_string(),
                        ),
                        ..SessionEventFields::default()
                    },
                )
                .await;
                return Ok(SegmentPublication::Failed(
                    PlaylistPublicationError::for_session(failure, &session),
                ));
            }
            if let Some((failure, published_segment)) = session
                .published_producer_ended_before(producer_attempt, deadline)
                .await
            {
                let beyond_frontier = producer_request_beyond_frontier(idx, published_segment);
                if beyond_frontier {
                    let waited_ms =
                        started_waiting.elapsed().as_millis().min(i64::MAX as u128) as i64;
                    tracing::warn!(
                        session = %session_log_id(session_id),
                        segment = name,
                        requested_segment = ?idx,
                        published_segment = ?published_segment,
                        waited_ms,
                        reason = failure.code(),
                        "HLS request crossed the retained frontier of an ended published producer"
                    );
                    return Ok(SegmentPublication::Failed(
                        PlaylistPublicationError::for_session(failure, &session),
                    ));
                }
            }
            let timed_out = Instant::now() >= deadline;
            if timed_out {
                let waited_ms = started_waiting.elapsed().as_millis().min(i64::MAX as u128) as i64;
                let reason = "producer_timeout";
                tracing::error!(
                    session = %session_log_id(session_id),
                    segment = name,
                    waited_ms,
                    reason,
                    progress_idle_ms = session
                        .progress
                        .stalled_for()
                        .as_millis()
                        .min(i64::MAX as u128) as i64,
                    "HLS segment was not available within the delivery window"
                );
                self.emit_session_event(
                    session_id,
                    &session,
                    "segment_unavailable",
                    SessionEventFields {
                        reason: Some(reason),
                        ms: Some(waited_ms),
                        extra: Some(
                            serde_json::json!({
                                "segment": name,
                                "progress_idle_ms": session
                                    .progress
                                    .stalled_for()
                                    .as_millis()
                                    .min(i64::MAX as u128) as i64
                            })
                            .to_string(),
                        ),
                        ..SessionEventFields::default()
                    },
                )
                .await;
                return Ok(SegmentPublication::Pending(current_owner()));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Compatibility facade for internal probes and older tests. HTTP serving
    /// uses [`Self::segment_for_publication`] so negative outcomes retain their
    /// exact response owner and typed failure classification.
    #[cfg(test)]
    pub async fn segment(
        self: &Arc<Self>,
        session_id: &str,
        name: &str,
    ) -> Result<Option<SegmentFile>, SegmentOpenError> {
        self.segment_for_publication(session_id, name)
            .await
            .map(|outcome| match outcome {
                SegmentPublication::Ready(file) => Some(*file),
                SegmentPublication::Missing(_)
                | SegmentPublication::Unavailable(_)
                | SegmentPublication::Pending(_)
                | SegmentPublication::Failed(_) => None,
            })
    }
}
