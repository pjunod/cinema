use super::*;

impl VodServe {
    /// Capture one exact admitted predecessor under one registry read. Recipe
    /// descriptions alone cannot mint this executable or its capacity.
    #[cfg(any(target_os = "linux", test))]
    pub(crate) async fn capture_processed_root(
        &self,
        request: &SessionRequest,
        supersession_user: &str,
        scope: [u8; 32],
        deadline: Instant,
    ) -> Option<PreparedRootAdmission> {
        let sessions = self.shared.sessions.lock().await;
        let id = request.previous_session_id.as_deref()?;
        let session = sessions.get(id)?;
        Self::capture_session_processed_root(
            session,
            id,
            request,
            supersession_user,
            scope,
            deadline,
        )
    }

    /// Called only with the manager's verified staged-ledger predecessor.
    /// Capture descriptive provenance and physical custody atomically. The
    /// caller re-derives scope from current authoritative inputs before attach.
    #[cfg(target_os = "linux")]
    pub(crate) async fn capture_staged_processed_root(
        &self,
        request: &SessionRequest,
        predecessor: &StagedProcessedPredecessor,
        supersession_user: &str,
        deadline: Instant,
    ) -> Option<(ProcessedReuseSeed, PreparedRootAdmission)> {
        let sessions = self.shared.sessions.lock().await;
        let session = sessions.get(&predecessor.session_id)?;
        let seed = session.processed_reuse_seed.clone()?;
        if seed.request_incarnation_id != predecessor.incarnation_id
            || seed.owner_node_id != predecessor.owner_node_id
            || request.request_id.as_deref() != Some(predecessor.successor_incarnation_id.as_str())
            || predecessor.session_id == predecessor.successor_session_id
            || predecessor.incarnation_id == predecessor.successor_incarnation_id
        {
            return None;
        }
        let root = Self::capture_session_processed_root(
            session,
            &predecessor.session_id,
            request,
            supersession_user,
            session.processed_reuse_scope?,
            deadline,
        )?;
        Some((seed, root))
    }

    #[cfg(any(target_os = "linux", test))]
    fn capture_session_processed_root(
        session: &Session,
        id: &str,
        request: &SessionRequest,
        supersession_user: &str,
        scope: [u8; 32],
        deadline: Instant,
    ) -> Option<PreparedRootAdmission> {
        if !session.renewable()
            || session.supersession_user != supersession_user
            || session.playback_id != request.playback_id
            || session.file.id != request.file_id
            || session.processed_reuse_scope != Some(scope)
            || Instant::now() >= deadline
        {
            return None;
        }
        let rendition = Arc::clone(session.live_rendition()?);
        let encoding = Arc::clone(rendition.recipe.encoding.as_ref()?);
        let permit = session.root_admission.clone()?;
        let reused_parent = ProcessedRootReuse {
            session_id: id.to_owned(),
            scope,
            owner: session.response_owner(),
            rendition: Arc::clone(&rendition),
            deadline,
        };
        if !rendition.ephemeral_processing || !reused_parent.matches(session, &permit) {
            return None;
        }
        Some(PreparedRootAdmission {
            reuse_scope: Some(scope),
            reused_parent: Some(reused_parent),
            instance_allowance: rendition._processed_instance_allowance.clone(),
            instance_nonce: None,
            encoding,
            permit,
        })
    }

    /// Every active VOD handle for the operator activity/session inventory.
    /// Tombstones remain addressable long enough to return their typed 410,
    /// but they are no longer deliveries and therefore stay out of this list.
    pub async fn delivery_infos(&self) -> Vec<VodDeliveryInfo> {
        self.delivery_infos_bounded(usize::MAX).await
    }

    /// Newest active VOD handles, bounded before they leave the registry.
    /// Cluster activity snapshots use this form so a node cannot make peer
    /// diagnostics enumerate more sessions than the wire contract can carry.
    pub async fn delivery_infos_bounded(&self, limit: usize) -> Vec<VodDeliveryInfo> {
        // Copy identities/control synchronously, sort and truncate before any
        // per-rendition await. Holding an Arc binds each observation to its row.
        let mut selected = {
            let sessions = self.shared.sessions.lock().await;
            sessions
                .iter()
                .filter(|(_, session)| session.tombstone.is_none())
                .filter_map(|(id, session)| {
                    let rendition = Arc::clone(session.live_rendition()?);
                    let snapshot = session.last_control_snapshot.as_ref();
                    let observation = VodActivityObservation {
                        display_target: display_target_activity(&rendition),
                        processing: processing_activity(&rendition),
                        control_demand: snapshot.map(|s| {
                            match s.demand {
                                crate::playback_control::PlaybackDemand::Active => "active",
                                crate::playback_control::PlaybackDemand::Hold => "hold",
                                crate::playback_control::PlaybackDemand::End => "end",
                            }
                            .to_owned()
                        }),
                        render_state: snapshot.map(|s| {
                            match s.render_state {
                                crate::playback_control::RenderState::Starting => "starting",
                                crate::playback_control::RenderState::Rendering => "rendering",
                                crate::playback_control::RenderState::Waiting => "waiting",
                                crate::playback_control::RenderState::Stalled => "stalled",
                                crate::playback_control::RenderState::Seeking => "seeking",
                                crate::playback_control::RenderState::Ended => "ended",
                                crate::playback_control::RenderState::Failed => "failed",
                            }
                            .to_owned()
                        }),
                        position_ms: snapshot.map(|s| s.position_ms),
                        client_runway_ms: snapshot
                            .map(|s| s.buffered_through_ms.saturating_sub(s.position_ms)),
                        control_age_ms: session
                            .control_observed_at
                            .map(|at| u64::try_from(at.elapsed().as_millis()).unwrap_or(u64::MAX)),
                        ..Default::default()
                    };
                    Some((
                        VodDeliveryInfo {
                            observation: Some(observation),
                            id: id.clone(),
                            method: match session.kind {
                                SessionKind::Copy { .. } => crate::delivery::Method::HlsCopy,
                                SessionKind::Transcode { .. } => crate::delivery::Method::Transcode,
                            },
                            file_id: session.file.id,
                            item_id: session.file.item_id,
                            item_title: session.item_title.clone(),
                            user_name: session.user_name.clone(),
                            target_height: session.target_height,
                            started_unix: session.started_unix,
                            idle_seconds: session
                                .last_touch
                                .lock()
                                .expect("touch lock")
                                .elapsed()
                                .as_secs(),
                            delivered_bytes: session.delivery.total_bytes(),
                            delivered_bps: session.delivery.recent_bps().map(|bytes| bytes * 8),
                            delivered_idle_ms: session.delivery.idle_for_ms(),
                        },
                        rendition,
                    ))
                })
                .collect::<Vec<_>>()
        };
        selected.sort_by(|(left, _), (right, _)| {
            right
                .started_unix
                .cmp(&left.started_unix)
                .then(left.id.cmp(&right.id))
        });
        selected.truncate(limit);
        let mut infos = Vec::with_capacity(selected.len());
        for (mut info, rendition) in selected {
            // Diagnostics never queue for a producer or acquire its manifest.
            let belief = rendition.slot.try_belief();
            let (state, hold) = if rendition.failure().is_some() {
                ("failed", None)
            } else {
                match belief {
                    Some(Producer::Running { .. }) => ("running", None),
                    Some(Producer::Stopped { reason, .. }) => (
                        "held",
                        Some(match reason {
                            crate::prodsched::Hold::Ahead { .. } => "ahead",
                            crate::prodsched::Hold::WorkingSetFull { .. } => "working_set",
                            crate::prodsched::Hold::NoRoom { .. } => "no_room",
                        }),
                    ),
                    Some(Producer::Absent { .. }) => (
                        "absent",
                        match *rendition.capacity_hold.lock().expect("capacity hold") {
                            Some(crate::prodsched::Hold::NoRoom { .. }) => Some("no_room"),
                            Some(crate::prodsched::Hold::WorkingSetFull { .. }) => {
                                Some("working_set")
                            }
                            _ => None,
                        },
                    ),
                    None => ("unknown", None),
                }
            };
            if let Some(observation) = info.observation.as_mut() {
                observation.producer_state = Some(state.to_owned());
                observation.producer_hold = hold.map(str::to_owned);
            }
            infos.push(info);
        }
        infos
    }

    pub async fn active_sessions(&self) -> usize {
        self.shared
            .sessions
            .lock()
            .await
            .values()
            .filter(|session| session.live_rendition().is_some())
            .count()
    }

    /// Consume the shipped clients' historical hard-coded marker miss only
    /// when it can be correlated to exactly one live VOD playback that was
    /// armed while inside a stored marker. The beacon is the durable skip
    /// intent that transient `Seeking` snapshots cannot provide: reporters
    /// may coalesce those away before the next control exchange.
    pub async fn consume_marker_prewarm_placeholder(
        &self,
        user_id: i64,
        file_id: i64,
        method: &str,
    ) -> bool {
        if !matches!(method, "remux" | "transcode") {
            return false;
        }
        let user_scope = serde_json::json!(["user_id", user_id]).to_string();
        let candidates = {
            let sessions = self.shared.sessions.lock().await;
            sessions
                .iter()
                .filter(|(_, session)| {
                    session.file.id == file_id && session.supersession_user == user_scope
                })
                .map(|(session_id, session)| {
                    (
                        session_id.clone(),
                        session.live_rendition().cloned(),
                        session.kind,
                    )
                })
                .collect::<Vec<_>>()
        };

        let [(session_id, rendition, kind)] = candidates.as_slice() else {
            // Ambiguity is a miss, never permission to steal another
            // playback's credit. Direct play and rolling HLS also land here.
            return false;
        };
        let Some(rendition) = rendition else {
            // A recent terminal VOD row can own a delayed beacon even though
            // its reader is already gone. Keep the miss rather than letting a
            // surviving same-file session steal it.
            return false;
        };
        if !matches!(
            (*kind, method),
            (SessionKind::Copy { .. }, "remux") | (SessionKind::Transcode { .. }, "transcode")
        ) {
            return false;
        }
        let reader_facts = {
            let readers = rendition.readers.lock().await;
            readers
                .get(session_id)
                .map(|reader| Arc::clone(&reader.marker_prewarm))
        };
        let Some(ledger) = reader_facts else {
            return false;
        };
        if !ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .can_match_client_skip()
        {
            return false;
        }

        let manifest = rendition.manifest.lock().await;
        let publications = rendition
            .publication_versions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut ledger = ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let result = ledger.note_client_skip(&manifest, &publications);
        drop(ledger);
        drop(publications);
        drop(manifest);
        if let Some(outcome) = result.outcome {
            self.emit_marker_prewarm(session_id, file_id, *kind, outcome);
        }
        result.matched
    }

    /// Renew one immutable session only after its caller has a concrete
    /// playlist, subtitle, or media response ready. Kept separate from
    /// lookup so a vanished/tombstoned capability cannot be mistaken for a
    /// successful rolling-to-VOD fallthrough.
    pub async fn commit_resolved_media(
        &self,
        session_id: &str,
        owner: &ResponseOwner,
        segment_index: Option<u32>,
    ) -> bool {
        // End/reattach uses this same per-incarnation gate. Whichever enters
        // first finishes its whole transition before the other can inspect
        // the registry, so a served frontier cannot reappear after a
        // tombstone detached its reader.
        let _lifecycle = owner.lifecycle.lock().await;
        let mut sessions = self.shared.sessions.lock().await;
        let Some(session) = sessions.get_mut(session_id) else {
            return false;
        };
        if !session.renewable()
            || owner.tombstone.is_some()
            || !Arc::ptr_eq(&session.lifecycle, &owner.lifecycle)
            || !Arc::ptr_eq(&session.incarnation, &owner.incarnation)
        {
            return false;
        }
        let (Some(session_rendition), Some(owner_rendition)) =
            (session.rendition.as_ref(), owner.rendition.as_ref())
        else {
            return false;
        };
        if !Arc::ptr_eq(session_rendition, owner_rendition) {
            return false;
        }
        if !session.owns_response_media(owner) {
            return false;
        }
        let (owner_rendition, reader_id) = owner
            .media_child
            .as_ref()
            .map(|child| (&child.rendition, child.reader_id.as_str()))
            .unwrap_or((owner_rendition, session_id));
        let mut readers = if segment_index.is_some() {
            Some(owner_rendition.readers.lock().await)
        } else {
            None
        };

        if owner.media_child.is_some()
            && readers
                .as_ref()
                .is_some_and(|readers| !readers.contains_key(reader_id))
        {
            return false;
        }

        // Every await is above this line. Touch and frontier advance are one
        // cancellation-safe commit: EOF can never renew a session without
        // also recording the exact served frontier (or vice versa).
        if let Some(grant) = session.passive_grant.as_ref() {
            let frontier = segment_index
                .and_then(|index| owner_rendition.plan.entry(index))
                .map(|entry| ticks_to_ms(entry.end_ticks(), owner_rendition.timescale));
            if !grant.delivered(frontier) {
                return false;
            }
        }
        *session.last_touch.lock().expect("touch lock") = Instant::now();
        if let (Some(index), Some(readers)) = (segment_index, readers.as_mut()) {
            if let Some(reader) = readers.get_mut(reader_id) {
                reader.served(index);
            }
        }
        // Child downloads describe supply only. They cannot settle the
        // public parent's marker/presentation observations.
        let marker_ledger = segment_index
            .filter(|_| owner.media_child.is_none())
            .and_then(|_| {
                readers
                    .as_ref()?
                    .get(session_id)
                    .map(|reader| Arc::clone(&reader.marker_prewarm))
            });
        let marker_identity = (session.file.id, session.kind);
        drop(readers);
        drop(sessions);
        if let (Some(index), Some(ledger)) = (segment_index, marker_ledger) {
            let manifest = owner_rendition.manifest.lock().await;
            let publications = owner_rendition
                .publication_versions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let outcome = ledger
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .settle_pending_at(index, &manifest, &publications);
            drop(publications);
            drop(manifest);
            if let Some(outcome) = outcome {
                self.emit_marker_prewarm(session_id, marker_identity.0, marker_identity.1, outcome);
            }
        }
        owner_rendition.kick();
        true
    }

    /// Confirm that a response token still names the live attachment without
    /// extending its lease. Streamed bodies use this immediately before
    /// publishing headers, then perform the mutating commit only after EOF.
    pub async fn response_owner_is_live(&self, session_id: &str, owner: &ResponseOwner) -> bool {
        let _lifecycle = owner.lifecycle.lock().await;
        let sessions = self.shared.sessions.lock().await;
        let Some(session) = sessions.get(session_id) else {
            return false;
        };
        session.renewable()
            && owner.tombstone.is_none()
            && Arc::ptr_eq(&session.lifecycle, &owner.lifecycle)
            && Arc::ptr_eq(&session.incarnation, &owner.incarnation)
            && session.owns_response_media(owner)
            && session.rendition.as_ref().is_some_and(|rendition| {
                owner
                    .rendition
                    .as_ref()
                    .is_some_and(|owner| Arc::ptr_eq(rendition, owner))
            })
    }

    /// Read-only Source telemetry gate. The publication must still belong to
    /// this exact live attachment and complete immutable Source assignment.
    pub(crate) async fn source_status_owner_is_current(
        &self,
        session_id: &str,
        owner: &ResponseOwner,
        assignment: &plurx_core::sharing_source_sessions::SourceDispatchAssignment,
    ) -> bool {
        let _lifecycle = owner.lifecycle.lock().await;
        let sessions = self.shared.sessions.lock().await;
        let Some(session) = sessions.get(session_id) else {
            return false;
        };
        session.tombstone.is_none()
            && owner.tombstone.is_none()
            && Arc::ptr_eq(&session.lifecycle, &owner.lifecycle)
            && Arc::ptr_eq(&session.incarnation, &owner.incarnation)
            && session.rendition_key == owner.rendition_key
            && session.supersession_user == assignment.binding().principal().owner_key()
            && session.rendition.as_ref().is_some_and(|rendition| {
                owner
                    .rendition
                    .as_ref()
                    .is_some_and(|owned| Arc::ptr_eq(rendition, owned))
                    && rendition.source_owners.contains_live_assignment(assignment)
            })
    }

    /// Frozen source facts carried by this exact VOD response owner. HTTP may
    /// prepare a representation from them before final owner admission without
    /// consulting whichever attachment currently reuses the public id.
    pub(crate) fn response_owner_file(&self, owner: &ResponseOwner) -> MediaFile {
        owner.file.as_ref().clone()
    }

    /// Feed a retained-reader fixture through the real owner-local acceptance
    /// state machine. No test-only proof or capacity claim is manufactured.
    #[cfg(test)]
    pub(crate) async fn accept_cached_wait_for_test(
        &self,
        route: &plurx_core::domain::MediaSessionRoute,
        request: &crate::playback_control::ControlRequestV1,
        successor: &str,
    ) {
        self.accept_cached_wait_while_sessions_held_for_test(route, request, successor, async {})
            .await;
    }

    #[cfg(test)]
    pub(crate) async fn accept_cached_wait_while_sessions_held_for_test(
        &self,
        route: &plurx_core::domain::MediaSessionRoute,
        request: &crate::playback_control::ControlRequestV1,
        successor: &str,
        observation: impl std::future::Future<Output = ()>,
    ) {
        tokio::time::sleep(Duration::from_millis(250)).await;
        let mut sessions = self.shared.sessions.lock().await;
        {
            let session = sessions
                .get_mut(&route.session_id)
                .expect("actual retained attachment");
            let first = session.last_control_snapshot.is_none();
            session.playback_id = route.playback_id.clone();
            let snapshot = crate::playback_control::PlaybackDemandSnapshot::from(request);
            let mut control = session.control.lock().expect("control lock");
            let result = control
                .accept(
                    &route.incarnation_id,
                    u64::try_from(route.owner_epoch).expect("owner epoch"),
                    &request.client_instance_id,
                    request.sequence,
                    crate::playback_control::ControlAcceptance::observed(
                        snapshot.platform(),
                        &crate::playback_control::PreparedSuccessorObservation::NotRequested,
                        None,
                        snapshot.request_fingerprint.as_deref(),
                        &snapshot.selection,
                        snapshot.capabilities.as_ref(),
                    ),
                )
                .expect("actual owner-local acceptance");
            assert_eq!(
                result.0,
                crate::playback_control::ControlDisposition::Accepted
            );
            control.observe(&snapshot.selection, snapshot.capabilities.as_ref());
            if first {
                assert!(control.stage_preparation(
                    successor.to_owned(),
                    route.incarnation_id.clone(),
                    crate::media_sessions::unix_ms() + 30_000,
                    Some(request.selection.desired().digest()),
                ));
            }
            session.last_control_snapshot = Some(snapshot);
        }
        // Keep only the real async sessions mutex across the observation.
        // The accepted-control std mutex is released before awaiting.
        observation.await;
    }

    #[cfg(test)]
    pub(crate) async fn while_cached_recovery_lock_held_for_test(
        &self,
        session_id: &str,
        lock: &str,
        observation: impl std::future::Future<Output = ()>,
    ) {
        match lock {
            "sessions" => {
                let _held = self.shared.sessions.lock().await;
                observation.await;
            }
            "readers" => {
                let rendition = self
                    .shared
                    .sessions
                    .lock()
                    .await
                    .get(session_id)
                    .expect("actual retained session")
                    .rendition
                    .clone()
                    .expect("actual retained rendition");
                let _held = rendition.readers.lock().await;
                observation.await;
            }
            _ => panic!("finite actual cached-recovery lock fixture"),
        }
    }

    /// Capture only an exact local accepted predecessor. The outer None is
    /// lock contention; Some(None) is definite lack of accepted authority.
    pub(crate) fn cached_prepared_wait_fence(
        &self,
        route: &plurx_core::domain::MediaSessionRoute,
        successor: &str,
        request: &crate::playback_control::ControlRequestV1,
    ) -> Option<Option<crate::playback_control::AcceptedControlFence>> {
        let sessions = self.shared.sessions.try_lock().ok()?;
        let Some(session) = sessions.get(&route.session_id) else {
            return Some(None);
        };
        if !session.renewable() || session.playback_id != route.playback_id {
            return Some(None);
        }
        let control = match session.control.try_lock() {
            Ok(control) => control,
            Err(std::sync::TryLockError::WouldBlock) => return None,
            Err(std::sync::TryLockError::Poisoned(_)) => return Some(None),
        };
        if control.live_preparation_incarnation() != Some(successor) {
            return Some(None);
        }
        Some((|| {
            control.accepted_observation(crate::playback_control::AcceptedControlIdentity {
                generation: route.incarnation_id.clone(),
                owner_epoch: u64::try_from(route.owner_epoch).ok()?,
                client_instance_id: request.client_instance_id.clone(),
                sequence: request.sequence,
                fingerprint: request.fingerprint()?,
                desired_digest: request.selection.desired().digest(),
            })
        })())
    }

    /// Unknown is lock contention only, never new accepted authority.
    pub(crate) fn cached_prepared_wait_is_current(
        &self,
        predecessor: &str,
        successor: &str,
        fence: &crate::playback_control::AcceptedControlFence,
        intent: Option<&plurx_core::playback::MediaIntentEnvelope>,
    ) -> Option<bool> {
        let Some(intent) = intent else {
            return Some(false);
        };
        let sessions = self.shared.sessions.try_lock().ok()?;
        let Some(session) = sessions
            .get(predecessor)
            .filter(|session| session.renewable())
        else {
            return Some(false);
        };
        let Some(snapshot) = session.last_control_snapshot.as_ref() else {
            return Some(false);
        };
        if snapshot.intent.as_ref() != Some(intent)
            || snapshot.demand != crate::playback_control::PlaybackDemand::Active
            || !matches!(
                snapshot.render_state,
                crate::playback_control::RenderState::Waiting
                    | crate::playback_control::RenderState::Stalled
            )
        {
            return Some(false);
        }
        let control = match session.control.try_lock() {
            Ok(control) => control,
            Err(std::sync::TryLockError::WouldBlock) => return None,
            Err(std::sync::TryLockError::Poisoned(_)) => return Some(false),
        };
        Some(
            control.live_preparation_incarnation() == Some(successor)
                && control.observation_is_current(fence),
        )
    }

    /// Issue a private proof only for this exact completed retained reader.
    /// The immutable artifact serves all bytes; its authority-only consumer
    /// cannot request an encoder permit or borrow the incumbent's producer.
    pub(crate) async fn cached_prepared_reader_owner(
        &self,
        session_id: &str,
        incarnation: &str,
        expected: &crate::transcode::RetainedOutputFacts,
    ) -> Option<ResponseOwner> {
        let sessions = self.shared.sessions.lock().await;
        let session = sessions.get(session_id)?;
        if !session.renewable()
            || session.prepared_incarnation.as_deref() != Some(incarnation)
            || !session.children.is_empty()
            || session.retained_output.as_ref()?.facts() != *expected
        {
            return None;
        }
        let rendition = session.rendition.as_ref()?;
        let readers = rendition.readers.lock().await;
        let reader = readers.get(session_id)?;
        if !reader.authority_only
            || reader.draining_after_prepared_commit
            || reader.admission_priority != crate::admission::Priority::Speculative
        {
            return None;
        }
        Some(session.response_owner())
    }

    /// Waiting cancellation cannot suspend or extend an accepted exchange.
    /// None is a contended lock, never proof to mint or renew cached authority.
    pub(crate) fn cached_prepared_reader_is_live(
        &self,
        session_id: &str,
        incarnation: &str,
        owner: &ResponseOwner,
    ) -> Option<bool> {
        if owner.tombstone.is_some() {
            return Some(false);
        }
        let _lifecycle = owner.lifecycle.try_lock().ok()?;
        let sessions = self.shared.sessions.try_lock().ok()?;
        let Some(session) = sessions.get(session_id) else {
            return Some(false);
        };
        if !session.renewable()
            || session.prepared_incarnation.as_deref() != Some(incarnation)
            || !Arc::ptr_eq(&session.lifecycle, &owner.lifecycle)
            || !Arc::ptr_eq(&session.incarnation, &owner.incarnation)
            || !session.children.is_empty()
        {
            return Some(false);
        }
        let (Some(actual), Some(retained), Some(rendition), Some(frozen)) = (
            session.retained_output.as_ref(),
            owner.retained_output.as_ref(),
            session.rendition.as_ref(),
            owner.rendition.as_ref(),
        ) else {
            return Some(false);
        };
        if !Arc::ptr_eq(actual, retained) || !Arc::ptr_eq(rendition, frozen) {
            return Some(false);
        }
        let readers = rendition.readers.try_lock().ok()?;
        Some(readers.get(session_id).is_some_and(|reader| {
            reader.authority_only
                && !reader.draining_after_prepared_commit
                && reader.admission_priority == crate::admission::Priority::Speculative
        }))
    }

    pub(crate) async fn prepared_owner_matches(
        &self,
        session_id: &str,
        owner: &ResponseOwner,
        incarnation: &str,
    ) -> bool {
        let sessions = self.shared.sessions.lock().await;
        sessions.get(session_id).is_some_and(|session| {
            session.tombstone.is_none()
                && session.prepared_incarnation.as_deref() == Some(incarnation)
                && Arc::ptr_eq(&session.lifecycle, &owner.lifecycle)
                && Arc::ptr_eq(&session.incarnation, &owner.incarnation)
        })
    }

    /// Keep the exact attachment stable while its first private choice is sealed.
    /// No release-transition lock may be held by the caller.
    pub(crate) async fn lock_preparation_owner(
        &self,
        session_id: &str,
        owner: &ResponseOwner,
        incarnation: &str,
    ) -> Option<tokio::sync::OwnedMutexGuard<()>> {
        let guard = Arc::clone(&owner.lifecycle).lock_owned().await;
        let sessions = self.shared.sessions.lock().await;
        let session = sessions.get(session_id)?;
        if session.prepared_incarnation.as_deref() != Some(incarnation)
            || session.tombstone.is_some()
            || !Arc::ptr_eq(&session.lifecycle, &owner.lifecycle)
            || !Arc::ptr_eq(&session.incarnation, &owner.incarnation)
            || !session.owns_response_media(owner)
        {
            return None;
        }
        Some(guard)
    }

    /// Admit a typed VOD status against the exact live-or-terminal snapshot
    /// that produced it, without renewing the session or publishing media.
    pub async fn response_status_owner_is_current(
        &self,
        session_id: &str,
        owner: &ResponseOwner,
    ) -> bool {
        let _lifecycle = owner.lifecycle.lock().await;
        let sessions = self.shared.sessions.lock().await;
        let Some(session) = sessions.get(session_id) else {
            return false;
        };
        session.tombstone == owner.tombstone
            && Arc::ptr_eq(&session.lifecycle, &owner.lifecycle)
            && Arc::ptr_eq(&session.incarnation, &owner.incarnation)
            && session.rendition_key == owner.rendition_key
            && session.owns_response_media(owner)
            && (session.tombstone.is_some()
                || session.rendition.as_ref().is_some_and(|rendition| {
                    owner
                        .rendition
                        .as_ref()
                        .is_some_and(|owner| Arc::ptr_eq(rendition, owner))
                }))
    }

    /// Full immutable rendition with this attachment's fixed acquisition hint.
    /// The preferred start does not trim media or rebase its timestamps.
    pub async fn playlist(&self, session_id: &str) -> Option<VodPublication<Vec<u8>>> {
        self.playlist_with_acquisition_start(session_id, None).await
    }

    /// This response's measured acquisition position never changes the stored
    /// attachment hint or the shared rendition. Invalid hints retain baseline.
    pub async fn playlist_with_acquisition_start(
        &self,
        session_id: &str,
        acquisition_start_ms: Option<i64>,
    ) -> Option<VodPublication<Vec<u8>>> {
        let publication = self.session_rendition(session_id).await?;
        let (result, owner) = match publication.result {
            Ok((rendition, _, _)) => (
                Ok(playlist_with_acquisition_start(
                    &rendition.playlist,
                    publication.owner.preferred_start_ms,
                    acquisition_start_ms.filter(|_| {
                        publication.owner.scalar_playlist && publication.owner.media_child.is_none()
                    }),
                    plan_duration_ms(&rendition.plan),
                )),
                publication.owner,
            ),
            Err(error) => (Err(error), publication.owner),
        };
        Some(VodPublication { result, owner })
    }

    /// The three-outcome segment GET (plan §2.3). The outer `None` means this
    /// is not a VOD session. `result: Ok(None)` is a genuine miss from one
    /// exact attachment; HTTP must retain the adjacent owner through its
    /// bodyless 404 admission instead of reconstructing identity from the
    /// reusable session id.
    pub async fn segment(
        &self,
        session_id: &str,
        name: &str,
    ) -> Option<VodPublication<Option<SegmentReady>>> {
        self.segment_before(session_id, name, None).await
    }

    /// Resolve a master only for an owned continuous graph. Every advertised
    /// init is verified before exposure; waiting never holds a build gate.
    pub(crate) async fn continuous_master_before(
        &self,
        session_id: &str,
        deadline: Instant,
    ) -> Option<VodPublication<Option<Vec<u8>>>> {
        let publication = self
            .verified_continuous_family_before(session_id, deadline)
            .await?;
        let result = async {
            let Some(family) = publication.result? else {
                return Ok(None);
            };
            self.bind_family_description_before(session_id, &publication.owner, &family, deadline)
                .await?;
            family
                .family
                .master_playlist(&family.video_budgets, family.audio_budget.as_ref())
                .map(String::into_bytes)
                .map(Some)
                .map_err(|error| VodError::ProducerFailed(error.to_string()))
        }
        .await;
        Some(VodPublication {
            owner: publication.owner,
            result,
        })
    }

    pub(crate) async fn continuous_family_description_before(
        &self,
        session_id: &str,
        deadline: Instant,
    ) -> Option<VodPublication<Option<Vec<u8>>>> {
        let publication = self
            .verified_continuous_family_before(session_id, deadline)
            .await;
        let Some(publication) = publication else {
            let root = self.session_rendition(session_id).await?;
            return Some(VodPublication {
                owner: root.owner,
                result: root.result.map(|_| None),
            });
        };
        let result = async {
            let Some(family) = publication.result? else {
                return Ok(None);
            };
            self.bind_family_description_before(session_id, &publication.owner, &family, deadline)
                .await
                .map(Some)
        }
        .await;
        Some(VodPublication {
            owner: publication.owner,
            result,
        })
    }

    async fn bind_family_description_before(
        &self,
        session_id: &str,
        owner: &ResponseOwner,
        family: &VerifiedContinuousFamily,
        deadline: Instant,
    ) -> Result<Vec<u8>, VodError> {
        let bytes = family.description()?;
        let description: plurx_core::store::ContinuousFamilyDescription =
            serde_json::from_slice(&bytes)
                .map_err(|error| VodError::ProducerFailed(error.to_string()))?;
        let binding = async {
            if !self.response_owner_is_live(session_id, owner).await {
                return Ok(false);
            }
            let Some(route) = self.shared.store.media_session_route(session_id).await? else {
                return Ok(false);
            };
            let now_ms = now_ms();
            self.shared
                .store
                .bind_continuous_family(
                    &route.incarnation_id,
                    &route.owner_node_id,
                    route.owner_epoch,
                    &description,
                    now_ms,
                )
                .await
        };
        let remaining = deadline.saturating_duration_since(Instant::now());
        let accepted = tokio::time::timeout(remaining, binding)
            .await
            .map_err(|_| VodError::ProducerFailed("continuous family binding deadline".into()))?
            .map_err(|error: plurx_core::error::StoreError| {
                VodError::ProducerFailed(error.to_string())
            })?;
        if !accepted || !self.response_owner_is_live(session_id, owner).await {
            return Err(VodError::ProducerFailed(
                "durable continuous family changed or its owner retired".into(),
            ));
        }
        family.client_description()
    }

    pub(crate) async fn verified_continuous_family_before(
        &self,
        session_id: &str,
        deadline: Instant,
    ) -> Option<VodPublication<Option<VerifiedContinuousFamily>>> {
        let is_continuous = {
            let sessions = self.shared.sessions.lock().await;
            let session = sessions.get(session_id)?;
            session
                .live_rendition()?
                .recipe
                .encoding
                .as_ref()
                .is_some_and(|encoding| {
                    encoding.shared_audio.is_none()
                        && !encoding.plan.options().input_has_audio
                        && encoding.options.video_sample_envelope
                            == plurx_core::transcode::VideoSampleEnvelope::ContinuousAvcHigh50
                })
        };
        if !is_continuous {
            return None;
        }
        let publication = self.session_rendition(session_id).await?;
        let owner = publication.owner;
        let result =
            async {
                let (_, _, _) = publication.result?;
                let children = {
                    let sessions = self.shared.sessions.lock().await;
                    let Some(session) = sessions.get(session_id) else {
                        return Ok(None);
                    };
                    if !Arc::ptr_eq(&session.incarnation, &owner.incarnation)
                        || !session.owns_response_media(&owner)
                        || session
                            .children
                            .iter()
                            .any(|child| child._reservation.is_none() && !child.controlled)
                    {
                        return Ok(None);
                    }
                    session
                        .children
                        .iter()
                        .map(|child| Arc::clone(&child.rendition))
                        .collect::<Vec<_>>()
                };
                let mut verified = Vec::new();
                for rendition in &children {
                    let Some(lookup) = self
                        .session_media_rendition(session_id, Some(&rendition.key))
                        .await
                    else {
                        return Ok(None);
                    };
                    let Some(found) = lookup.result? else {
                        return Ok(None);
                    };
                    if !Arc::ptr_eq(&lookup.owner.incarnation, &owner.incarnation)
                        || !Arc::ptr_eq(&found.rendition, rendition)
                    {
                        return Ok(None);
                    }
                    let budget = found
                        .block_budget
                        .min(deadline.saturating_duration_since(Instant::now()));
                    let mut ready = self.serve_init(rendition, budget, found.delivery).await?;
                    let expected = rendition
                        .identity
                        .lock()
                        .await
                        .identity
                        .as_ref()
                        .map(|identity| identity.served_init.clone());
                    let Some(expected) = expected else {
                        return Ok(None);
                    };
                    let Some(bytes) = read_child_init(&mut ready, &expected).await? else {
                        return Ok(None);
                    };
                    let mut reader = FragmentReader::new();
                    reader.push(&bytes);
                    let init = match reader.next_unit() {
                        Ok(Some(Unit::Init(init))) if reader.buffered() == 0 => init,
                        _ => {
                            return Err(VodError::ProducerFailed(
                                "family init is not one verified record".into(),
                            ))
                        }
                    };
                    verified.push((Arc::clone(rendition), expected, init));
                }
                let mut keys = children
                    .iter()
                    .map(|rendition| rendition.key.as_str())
                    .collect::<Vec<_>>();
                keys.sort_unstable();
                keys.dedup();
                if keys.len() != children.len() {
                    return Ok(None);
                }
                let mut guards = Vec::new();
                for key in keys {
                    guards.push(self.shared.rendition_build_gate(key).lock_owned().await);
                }
                for (rendition, expected, _) in &verified {
                    if rendition.closed.load(Relaxed)
                        || rendition.failure().is_some()
                        || self.source_changed(rendition)
                        || rendition
                            .identity
                            .lock()
                            .await
                            .identity
                            .as_ref()
                            .is_none_or(|identity| identity.served_init != *expected)
                    {
                        return Err(VodError::ProducerFailed(
                            "family source or init changed during verification".into(),
                        ));
                    }
                }
                let mut audio = None;
                let mut audio_budget = None;
                let mut rungs = Vec::new();
                let mut video_budgets = Vec::new();
                let fail = |error: plurx_core::fmp4::Fmp4Error| {
                    VodError::ProducerFailed(error.to_string())
                };
                for (rendition, _, init) in &verified {
                    let encoding =
                        rendition.recipe.encoding.as_ref().ok_or_else(|| {
                            VodError::ProducerFailed("family recipe missing".into())
                        })?;
                    if let Some(recipe) = &encoding.shared_audio {
                        if audio.is_some() {
                            return Err(VodError::ProducerFailed(
                                "family has multiple soundtracks".into(),
                            ));
                        }
                        audio = Some(
                            plurx_core::transcode::VodSharedAudioRendition::from_verified_init(
                                &encoding.plan,
                                recipe,
                                init,
                                &rendition.key,
                                &encoding.source_object_version,
                            )
                            .map_err(fail)?,
                        );
                        audio_budget = Some(plurx_core::transcode::VodRenditionBandwidth {
                            rendition_id: rendition.key.clone(),
                            average_bps: None,
                            peak_bps: encoding.continuous_peak_bps(&rendition.plan).ok_or_else(
                                || VodError::ProducerFailed("audio delivery budget missing".into()),
                            )?,
                            sustained_bps: encoding.continuous_sustained_bps(),
                        });
                    }
                }
                for (rendition, _, init) in &verified {
                    let encoding = rendition.recipe.encoding.as_ref().expect("verified recipe");
                    if encoding.shared_audio.is_some() {
                        continue;
                    }
                    rungs.push(
                        plurx_core::transcode::VodVideoRung::from_verified_init(
                            &rendition.recipe.file,
                            &encoding.plan,
                            init,
                            encoding.grid,
                            &rendition.key,
                            &encoding.source_object_version,
                            audio.as_ref().map(|audio| audio.recipe_id()),
                        )
                        .map_err(fail)?,
                    );
                    video_budgets.push(plurx_core::transcode::VodRenditionBandwidth {
                        rendition_id: rendition.key.clone(),
                        average_bps: None,
                        peak_bps: encoding.continuous_peak_bps(&rendition.plan).ok_or_else(
                            || VodError::ProducerFailed("video delivery budget missing".into()),
                        )?,
                        sustained_bps: encoding.continuous_sustained_bps(),
                    });
                }
                let family = plurx_core::transcode::VodPresentationFamily::new(
                    plurx_core::transcode::VodVideoFamily::new(rungs).map_err(fail)?,
                    audio,
                )
                .map_err(fail)?;

                let sessions = self.shared.sessions.lock().await;
                let Some(session) = sessions.get(session_id) else {
                    return Ok(None);
                };
                if !Arc::ptr_eq(&session.incarnation, &owner.incarnation)
                    || !session.owns_response_media(&owner)
                    || session.children.len() != children.len()
                    || children.iter().any(|rendition| {
                        !session.children.iter().any(|child| {
                            Arc::ptr_eq(&child.rendition, rendition)
                                && (child._reservation.is_some() || child.controlled)
                        })
                    })
                {
                    return Ok(None);
                }
                let candidates = session
                    .children
                    .iter()
                    .filter_map(|child| {
                        child
                            .candidate_id
                            .map(|id| (child.rendition.key.clone(), id))
                    })
                    .collect();
                Ok(Some(VerifiedContinuousFamily {
                    controlled: session.children.iter().any(|child| child.controlled),
                    family,
                    video_budgets,
                    audio_budget,
                    candidates,
                }))
            }
            .await;
        Some(VodPublication { result, owner })
    }

    pub(crate) async fn child_playlist_before(
        &self,
        session_id: &str,
        role: &str,
        rendition_id: &str,
        deadline: Instant,
    ) -> Option<VodPublication<Option<Vec<u8>>>> {
        if !ChildMediaRequest::valid_identity(role, rendition_id) {
            return None;
        }
        let publication = self
            .session_media_rendition(session_id, Some(rendition_id))
            .await?;
        let owner = publication.owner;
        let result = async {
            let Some(found) = publication.result? else {
                return Ok(None);
            };
            let Some(encoding) = found.rendition.recipe.encoding.as_ref() else {
                return Ok(None);
            };
            if (role == "audio") != encoding.shared_audio.is_some() {
                return Ok(None);
            }
            let expected = found
                .rendition
                .identity
                .lock()
                .await
                .identity
                .as_ref()
                .map(|identity| identity.served_init.clone());
            let Some(expected) = expected else {
                return Ok(None);
            };
            let budget = found
                .block_budget
                .min(deadline.saturating_duration_since(Instant::now()));
            let mut ready = self
                .serve_init(&found.rendition, budget, found.delivery)
                .await?;
            let Some(bytes) = read_child_init(&mut ready, &expected).await? else {
                return Ok(None);
            };
            let mut reader = FragmentReader::new();
            reader.push(&bytes);
            let init = match reader.next_unit() {
                Ok(Some(Unit::Init(init))) if reader.buffered() == 0 => init,
                _ => {
                    return Err(VodError::ProducerFailed(
                        "continuous child init is not one verified record".into(),
                    ))
                }
            };
            let shared_audio_recipe = if role == "video"
                && !found.rendition.recipe.file.audio_streams.is_empty()
            {
                let sessions = self.shared.sessions.lock().await;
                let Some(session) = sessions.get(session_id) else {
                    return Ok(None);
                };
                if !Arc::ptr_eq(&session.incarnation, &owner.incarnation)
                    || !session.owns_response_media(&owner)
                {
                    return Ok(None);
                }
                let mut matching = session.children.iter().filter_map(|child| {
                    let audio_encoding = child.rendition.recipe.encoding.as_ref()?;
                    let audio = audio_encoding.shared_audio.as_ref()?;
                    (audio_encoding.source_object_version == encoding.source_object_version
                        && child.rendition.recipe.file.id == found.rendition.recipe.file.id
                        && child.rendition.recipe.audio_index == found.rendition.recipe.audio_index)
                        .then(|| audio.digest().to_owned())
                });
                let Some(recipe) = matching.next() else {
                    return Ok(None);
                };
                if matching.next().is_some() {
                    return Ok(None);
                }
                Some(recipe)
            } else {
                None
            };
            let playlist = if let Some(audio) = encoding.shared_audio.as_ref() {
                plurx_core::transcode::VodSharedAudioRendition::from_verified_init(
                    &encoding.plan,
                    audio,
                    &init,
                    &found.rendition.key,
                    &encoding.source_object_version,
                )
                .and_then(|audio| audio.media_playlist(&found.rendition.plan))
            } else {
                plurx_core::transcode::VodVideoRung::from_verified_init(
                    &found.rendition.recipe.file,
                    &encoding.plan,
                    &init,
                    encoding.grid,
                    &found.rendition.key,
                    &encoding.source_object_version,
                    shared_audio_recipe.as_deref(),
                )
                .and_then(|video| video.media_playlist(&found.rendition.plan))
            }
            .map_err(|error| VodError::ProducerFailed(error.to_string()))?;
            if self.source_changed(&found.rendition)
                || found
                    .rendition
                    .identity
                    .lock()
                    .await
                    .identity
                    .as_ref()
                    .is_none_or(|identity| identity.served_init != expected)
            {
                return Err(VodError::ProducerFailed(
                    "continuous child source or init changed during playlist verification".into(),
                ));
            }
            Ok(Some(playlist.into_bytes()))
        }
        .await;
        Some(VodPublication { result, owner })
    }

    /// Materialize at most two exact video entries at or after the client's
    /// append frontier, together with their shared AAC dependencies. Downloads
    /// use private demand identities and never move the parent's playhead.
    pub(crate) async fn quality_ready_before(
        &self,
        session_id: &str,
        family: &plurx_core::transcode::VodPresentationFamily,
        rendition_id: &str,
        timescale: u32,
        frontier: u64,
        deadline: Instant,
    ) -> Result<Vec<plurx_core::playback::continuous_quality::QualityInterval>, String> {
        let work = async {
            let rung = family
                .video()
                .rungs()
                .iter()
                .find(|rung| rung.rendition_id() == rendition_id)
                .ok_or("preparation target is outside the verified family")?;
            if timescale != rung.grid().numerator {
                return Err("append frontier clock changed".into());
            }
            let frontier_ms = (u128::from(frontier) * 1000 / u128::from(timescale.max(1)))
                .min(i64::MAX as u128) as i64;
            self.admit_controlled_video_before(session_id, rendition_id, frontier_ms, deadline)
                .await?;
            let publication = self
                .session_media_rendition(session_id, Some(rendition_id))
                .await
                .ok_or("preparation parent is not attached")?;
            let owner = publication.owner;
            let found = publication
                .result
                .map_err(|error| format!("{error:?}"))?
                .ok_or("preparation target is not a private child")?;
            let selected = found
                .rendition
                .plan
                .entries
                .iter()
                .filter(|entry| entry.start_ticks >= frontier)
                .take(2)
                .cloned()
                .collect::<Vec<_>>();
            if selected.is_empty() {
                return Err("append frontier is beyond the film".into());
            }
            let mut intervals = Vec::new();
            for entry in selected {
                let request = ChildMediaRequest {
                    role: "video".into(),
                    rendition: rendition_id.into(),
                    kind: "segment".into(),
                    object: format!("{}.m4s", entry.index),
                };
                let mut ready = self
                    .child_segment_with_wait_before(session_id, &request, deadline, true)
                    .await
                    .ok_or("preparation parent disappeared")?
                    .result
                    .map_err(|error| format!("{error:?}"))?
                    .ok_or("preparation fragment disappeared")?;
                if ready.len == 0 || ready.len > 16 * 1024 * 1024 {
                    return Err("preparation fragment exceeds its bounded inspection".into());
                }
                let mut media = Vec::new();
                (&mut ready.file)
                    .take(ready.len + 1)
                    .read_to_end(&mut media)
                    .await
                    .map_err(|error| error.to_string())?;
                if media.len() as u64 != ready.len {
                    return Err("preparation fragment length changed".into());
                }
                let (media, artifact_id) = quality_inspection(move || {
                    let artifact_id = hex::encode(Sha256::digest(&media));
                    (media, artifact_id)
                })
                .await?;
                let interval = plurx_core::playback::continuous_quality::QualityInterval {
                    artifact_id,
                    rendition_id: rendition_id.into(),
                    timescale,
                    from_tick: entry.start_ticks,
                    through_tick: entry.end_ticks(),
                    byte_length: ready.len,
                };
                // Build gates protect the verification snapshot, not producer
                // waits: publication itself needs these same gates.
                let _gate = self
                    .shared
                    .rendition_build_gate(rendition_id)
                    .lock_owned()
                    .await;
                let init = super::vod_serve_serve::read_quality_artifact(
                    &found.rendition.dir.path().join(INIT_NAME),
                    256 * 1024,
                )
                .await?;
                let (verified_rung, verified_interval) = (rung.clone(), interval.clone());
                quality_inspection(move || {
                    super::vod_serve_serve::verify_cached_quality_interval(
                        &init,
                        &media,
                        &verified_rung,
                        &verified_interval,
                    )
                })
                .await??;
                drop(_gate);
                if let Some(audio) = family.audio() {
                    let audio_lookup = self
                        .session_media_rendition(session_id, Some(audio.rendition_id()))
                        .await
                        .ok_or("soundtrack parent disappeared")?;
                    let soundtrack = audio_lookup
                        .result
                        .map_err(|error| format!("{error:?}"))?
                        .ok_or("soundtrack child disappeared")?;
                    let dependencies = family
                        .shared_audio_dependencies(
                            rendition_id,
                            &found.rendition.plan,
                            entry.index,
                            Some(&soundtrack.rendition.plan),
                        )
                        .map_err(|error| error.to_string())?;
                    for index in dependencies {
                        let request = ChildMediaRequest {
                            role: "audio".into(),
                            rendition: audio.rendition_id().into(),
                            kind: "segment".into(),
                            object: format!("{index}.m4s"),
                        };
                        let mut ready = self
                            .child_segment_with_wait_before(session_id, &request, deadline, true)
                            .await
                            .ok_or("soundtrack parent disappeared")?
                            .result
                            .map_err(|error| format!("{error:?}"))?
                            .ok_or("soundtrack dependency disappeared")?;
                        if ready.len == 0 || ready.len > 16 * 1024 * 1024 {
                            return Err("soundtrack inspection is oversized".into());
                        }
                        let mut bytes = Vec::new();
                        (&mut ready.file)
                            .take(ready.len + 1)
                            .read_to_end(&mut bytes)
                            .await
                            .map_err(|error| error.to_string())?;
                        if bytes.len() as u64 != ready.len {
                            return Err("soundtrack length changed".into());
                        }
                        let _gate = self
                            .shared
                            .rendition_build_gate(audio.rendition_id())
                            .lock_owned()
                            .await;
                        let init = super::vod_serve_serve::read_quality_artifact(
                            &soundtrack.rendition.dir.path().join(INIT_NAME),
                            256 * 1024,
                        )
                        .await?;
                        let entry = soundtrack
                            .rendition
                            .plan
                            .entry(index)
                            .ok_or("soundtrack plan changed")?;
                        let final_interval = index as usize + 1 == soundtrack.rendition.plan.len();
                        let (audio_timescale, from_tick, through_tick, byte_length) = (
                            soundtrack.rendition.timescale,
                            entry.start_ticks,
                            entry.end_ticks(),
                            ready.len,
                        );
                        let verified_audio = audio.clone();
                        quality_inspection(move || {
                            let dependency =
                                plurx_core::playback::continuous_quality::QualityInterval {
                                    artifact_id: hex::encode(Sha256::digest(&bytes)),
                                    rendition_id: verified_audio.rendition_id().into(),
                                    timescale: audio_timescale,
                                    from_tick,
                                    through_tick,
                                    byte_length,
                                };
                            super::vod_serve_serve::verify_cached_shared_audio_interval(
                                &init,
                                &bytes,
                                &verified_audio,
                                &dependency,
                                final_interval,
                            )
                        })
                        .await??;
                        if self.source_changed(&soundtrack.rendition) {
                            return Err("soundtrack source changed".into());
                        }
                    }
                }
                intervals.push(interval);
            }
            if self.source_changed(&found.rendition)
                || !self.response_owner_is_live(session_id, &owner).await
            {
                return Err("preparation owner or source changed".into());
            }
            Ok(intervals)
        };
        tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), work)
            .await
            .map_err(|_| "quality preparation exceeded its inherited deadline".to_owned())?
    }

    /// Resolve media through one exact parent's private reader. Role and init
    /// identity are checked before any demand can reach the producer.
    pub(crate) async fn child_segment_before(
        &self,
        session_id: &str,
        request: &ChildMediaRequest,
        deadline: Instant,
    ) -> Option<VodPublication<Option<SegmentReady>>> {
        self.child_segment_with_wait_before(session_id, request, deadline, false)
            .await
    }

    async fn child_segment_with_wait_before(
        &self,
        session_id: &str,
        request: &ChildMediaRequest,
        deadline: Instant,
        preparing: bool,
    ) -> Option<VodPublication<Option<SegmentReady>>> {
        let name = request.media_name()?;
        let publication = self
            .session_media_rendition(session_id, Some(&request.rendition))
            .await?;
        let owner = publication.owner;
        let found = match publication.result {
            Ok(Some(found)) => found,
            Ok(None) => {
                return Some(VodPublication {
                    result: Ok(None),
                    owner,
                })
            }
            Err(error) => {
                return Some(VodPublication {
                    result: Err(error),
                    owner,
                })
            }
        };
        let compatible_role = found
            .rendition
            .recipe
            .encoding
            .as_ref()
            .is_some_and(|encoding| {
                if request.role == "audio" {
                    encoding.shared_audio.is_some()
                } else {
                    encoding.shared_audio.is_none()
                        && !encoding.plan.options().input_has_audio
                        && encoding.plan.options().video_sample_envelope
                            == plurx_core::transcode::VideoSampleEnvelope::ContinuousAvcHigh50
                }
            });
        if !compatible_role {
            return Some(VodPublication {
                result: Ok(None),
                owner,
            });
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        // Optional preparation spends its inherited allowance once across
        // video and AAC. Ordinary media requests retain their configured cap.
        let budget = if preparing {
            remaining
        } else {
            found.block_budget.min(remaining)
        };
        if name == INIT_NAME {
            let expected = request.object.strip_suffix(".mp4")?;
            let matches_init = found
                .rendition
                .identity
                .lock()
                .await
                .identity
                .as_ref()
                .is_some_and(|identity| identity.served_init == expected);
            if !matches_init {
                return Some(VodPublication {
                    result: Ok(None),
                    owner,
                });
            }
            let result = self
                .serve_init(&found.rendition, budget, found.delivery)
                .await;
            // Materialization can restart a producer. Recheck the actual served
            // init after opening instead of labelling regenerated bytes by URI.
            let result = match result {
                Ok(mut ready) => {
                    if found
                        .rendition
                        .identity
                        .lock()
                        .await
                        .identity
                        .as_ref()
                        .is_some_and(|identity| identity.served_init == expected)
                    {
                        match read_child_init(&mut ready, expected).await {
                            Ok(Some(_)) => {}
                            Ok(None) => {
                                return Some(VodPublication {
                                    result: Ok(None),
                                    owner,
                                })
                            }
                            Err(error) => {
                                return Some(VodPublication {
                                    result: Err(error),
                                    owner,
                                })
                            }
                        }
                        ready.etag = expected.to_owned();
                        Ok(Some(ready))
                    } else {
                        Ok(None)
                    }
                }
                Err(error) => Err(error),
            };
            return Some(VodPublication { result, owner });
        }
        let index = planned_index(&name)?;
        if index as usize >= found.rendition.plan.len() {
            return Some(VodPublication {
                result: Ok(None),
                owner,
            });
        }
        let reader_id = &owner.media_child.as_ref()?.reader_id;
        let result = self
            .serve_segment_for(
                &found.rendition,
                (reader_id, session_id),
                index,
                budget,
                found.delivery,
            )
            .await
            .and_then(|ready| {
                if found
                    .rendition
                    .recipe
                    .encoding
                    .as_ref()
                    .is_some_and(|encoding| {
                        encoding.continuous_object_fits(&found.rendition.plan, index, ready.len)
                            == Some(false)
                    })
                {
                    Err(VodError::ProducerFailed(
                        "cached child exceeds its delivery budget".into(),
                    ))
                } else {
                    Ok(Some(ready))
                }
            });
        Some(VodPublication { result, owner })
    }

    /// Resolve a segment without allowing its blocked-GET allowance to run
    /// past a caller's response-publication deadline.
    pub(crate) async fn segment_before(
        &self,
        session_id: &str,
        name: &str,
        deadline: Option<Instant>,
    ) -> Option<VodPublication<Option<SegmentReady>>> {
        self.segment_with_read_custody(session_id, name, deadline, None)
            .await
    }

    pub(crate) async fn source_segment_before(
        &self,
        session_id: &str,
        name: &str,
        deadline: Option<Instant>,
        custody: crate::transcode::source_actor::resource::SourceResourceReadCustody,
    ) -> Option<VodPublication<Option<SegmentReady>>> {
        self.segment_with_read_custody(session_id, name, deadline, Some(&custody))
            .await
    }

    async fn segment_with_read_custody(
        &self,
        session_id: &str,
        name: &str,
        deadline: Option<Instant>,
        custody: Option<&crate::transcode::source_actor::resource::SourceResourceReadCustody>,
    ) -> Option<VodPublication<Option<SegmentReady>>> {
        let publication = self.session_rendition(session_id).await?;
        let (rendition, mut budget, delivery) = match publication.result {
            Ok(found) => found,
            Err(error) => {
                return Some(VodPublication {
                    result: Err(error),
                    owner: publication.owner,
                })
            }
        };
        if let Some(deadline) = deadline {
            budget = budget.min(deadline.saturating_duration_since(Instant::now()));
        }
        let owner = publication.owner;
        if let Some(artifact) = owner.retained_output.as_ref() {
            let index = if name == INIT_NAME {
                None
            } else {
                let Some(index) =
                    planned_index(name).filter(|index| (*index as usize) < rendition.plan.len())
                else {
                    return Some(VodPublication {
                        result: Ok(None),
                        owner,
                    });
                };
                Some(index)
            };
            let result = artifact
                .open(index, &delivery, &self.shared, &rendition, budget)
                .await
                .map(Some);
            return Some(VodPublication { result, owner });
        }
        if name == INIT_NAME {
            return Some(VodPublication {
                result: match custody {
                    Some(custody) => {
                        self.serve_init_with_read_custody(
                            &rendition,
                            budget,
                            delivery,
                            Some(custody),
                        )
                        .await
                    }
                    None => self.serve_init(&rendition, budget, delivery).await,
                }
                .map(Some),
                owner,
            });
        }
        let Some(index) = planned_index(name) else {
            // Traversal names and everything else that is not `segNNNNN.m4s`
            // fail the same digit discipline `is_safe_segment` enforces.
            return Some(VodPublication {
                result: Ok(None),
                owner,
            });
        };
        if index as usize >= rendition.plan.len() {
            return Some(VodPublication {
                result: Ok(None),
                owner,
            });
        }
        Some(VodPublication {
            result: match custody {
                Some(custody) => {
                    self.serve_segment_with_read_custody(
                        &rendition,
                        session_id,
                        index,
                        budget,
                        delivery,
                        Some(custody),
                    )
                    .await
                }
                None => {
                    self.serve_segment(&rendition, session_id, index, budget, delivery)
                        .await
                }
            }
            .map(Some),
            owner,
        })
    }
}

/// Hashing and parsing a bounded preparation fragment (up to 16 MiB of video,
/// plus its AAC dependencies) is CPU work, and up to 64 schedules can prepare
/// at once, so it runs on the blocking pool and the async workers keep
/// serving. The result is exactly what the same closure returns inline.
async fn quality_inspection<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| format!("quality inspection task failed: {error}"))
}

/// Validate the opened init, retaining the same descriptor for HTTP streaming.
async fn read_child_init(
    ready: &mut SegmentReady,
    expected: &str,
) -> Result<Option<Vec<u8>>, VodError> {
    if ready.len > 256 * 1024 {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    (&mut ready.file)
        .take(256 * 1024 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(VodError::Io)?;
    if bytes.len() as u64 != ready.len || format!("{:x}", Sha256::digest(&bytes)) != expected {
        return Ok(None);
    }
    ready
        .file
        .seek(std::io::SeekFrom::Start(0))
        .await
        .map_err(VodError::Io)?;
    Ok(Some(bytes))
}

/// HLS uses the start hint only for acquisition. The full playlist remains
/// seekable, and PRECISE=NO lets the reader begin at the preceding segment;
/// the player's ordinary ownership-fenced seek still establishes alignment.
pub(super) fn preferred_playlist_start_ms(start_seconds: f64, duration_ms: i64) -> Option<i64> {
    let milliseconds = start_seconds * 1_000.0;
    if !start_seconds.is_finite()
        || !milliseconds.is_finite()
        || milliseconds < 1.0
        || milliseconds >= duration_ms as f64
        || milliseconds >= i64::MAX as f64
    {
        return None;
    }
    Some(milliseconds.floor() as i64)
}

pub(super) fn playlist_with_preferred_start(playlist: &[u8], start_ms: Option<i64>) -> Vec<u8> {
    let Some(start_ms) = start_ms.filter(|start| *start > 0) else {
        return playlist.to_vec();
    };
    playlist_with_start_tag(playlist, start_ms)
}

pub(super) fn playlist_with_acquisition_start(
    playlist: &[u8],
    attachment_start_ms: Option<i64>,
    acquisition_start_ms: Option<i64>,
    duration_ms: i64,
) -> Vec<u8> {
    if let Some(start) = acquisition_start_ms.filter(|start| *start >= 0 && *start < duration_ms) {
        playlist_with_start_tag(playlist, start)
    } else {
        playlist_with_preferred_start(playlist, attachment_start_ms)
    }
}

fn playlist_with_start_tag(playlist: &[u8], start_ms: i64) -> Vec<u8> {
    // Rendition playlists are generated from validated immutable plans. Keep
    // all their bytes except an existing start tag; never rewrite media URIs,
    // durations, sequence numbers, discontinuities, or the ENDLIST promise.
    let Ok(text) = std::str::from_utf8(playlist) else {
        return playlist.to_vec();
    };
    let mut lines = text.split_inclusive('\n');
    let Some(header) = lines
        .next()
        .filter(|line| line.ends_with('\n') && line.trim_end() == "#EXTM3U")
    else {
        return playlist.to_vec();
    };
    let mut output = Vec::with_capacity(playlist.len() + 72);
    output.extend_from_slice(header.as_bytes());
    output.extend_from_slice(
        format!(
            "#EXT-X-START:TIME-OFFSET={}.{:03},PRECISE=NO\n",
            start_ms / 1_000,
            start_ms % 1_000,
        )
        .as_bytes(),
    );
    for line in lines.filter(|line| !line.trim_start().starts_with("#EXT-X-START:")) {
        output.extend_from_slice(line.as_bytes());
    }
    output
}

/// The processing a rendition's graph is doing, named for Activity. Read from
/// the frozen runtime and the selected plan, never from the request.
fn processing_activity(rendition: &Rendition) -> Option<crate::vodserve::ProcessingActivity> {
    #[cfg(target_os = "linux")]
    {
        use plurx_core::transcode::dv_processing::{DvProcessingMode, DvSelection};
        let encoding = rendition.recipe.encoding.as_ref()?;
        let runtime = encoding.dv_runtime.as_ref()?;
        let DvSelection::Selected(plan) = &encoding.dv_processing else {
            return None;
        };
        Some(processing_activity_text(
            plan.destination(),
            runtime.mode == DvProcessingMode::Fel && plan.reconstructs_fel(),
            runtime.decode == crate::dv_segment::RendererDecode::Gpu,
            &encoding.delivered_output_encoder(),
            rendition.ephemeral_processing,
        ))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = rendition;
        None
    }
}

#[cfg(any(target_os = "linux", test))]
pub(crate) fn processing_activity_text(
    destination: plurx_core::transcode::dv_processing::DvDestination,
    fel_composed: bool,
    gpu_decode: bool,
    encoder: &str,
    live_rendering: bool,
) -> crate::vodserve::ProcessingActivity {
    use plurx_core::transcode::dv_processing::DvDestination;
    let decode = if gpu_decode {
        "VA-API decode"
    } else {
        "software decode"
    };
    let picture = if fel_composed {
        "base and full enhancement layers composed on the GPU"
    } else {
        "base layer reshaped by the RPU on the GPU"
    };
    let (delivered, reason) = match destination {
        DvDestination::Profile81 => (
            "Reconstructed Dolby Vision Profile 8.1",
            "Dolby Vision client playing a Profile 7 FEL title; FEL re-encode is on in \
             Settings → Developer",
        ),
        DvDestination::Hdr10 => (
            "Processed HDR10 (HDR10-E)",
            "HDR client playing a Dolby Vision title; Dolby Vision processing is on in \
             Settings → Developer",
        ),
        DvDestination::Sdr => (
            "Processed SDR",
            "SDR display playing a Dolby Vision title; display targeting is on in \
             Settings → Developer",
        ),
    };
    let tail = if destination == DvDestination::Profile81 {
        ", a rewritten 8.1 RPU per frame"
    } else {
        ""
    };
    crate::vodserve::ProcessingActivity {
        delivered: delivered.to_owned(),
        work: format!("{decode}, {picture}, {encoder} Main10 encode{tail}"),
        reason: reason.to_owned(),
        fel_composed,
        live_rendering,
    }
}

fn display_target_activity(rendition: &Rendition) -> Option<DisplayTargetActivity> {
    #[cfg(target_os = "linux")]
    {
        let runtime = rendition.recipe.encoding.as_ref()?.dv_runtime.as_ref()?;
        if runtime.destination == plurx_core::transcode::dv_processing::DvDestination::Profile81 {
            return None;
        }
        if runtime.display_target_note.is_none() && runtime.display.is_none() {
            return None;
        }
        let note = runtime.display_target_note.clone().unwrap_or_else(|| {
            "Display targeting is off for this session; the master grade is sent.".into()
        });
        Some(DisplayTargetActivity {
            reported_peak_nits: runtime
                .display
                .as_ref()
                .and_then(|display| display.max_nits),
            peak_source: runtime
                .display
                .as_ref()
                .and_then(|display| display.peak_source),
            bucket_nits: runtime.display_target.map(|target| target.nits()),
            live_rendering: rendition.ephemeral_processing,
            note,
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = rendition;
        None
    }
}

impl VodServe {
    /// Observation only. Processed roots never enter retained storage; active
    /// readers and preparation reservations retain their existing ownership.
    pub(crate) async fn display_target_usage(
        &self,
    ) -> (usize, std::collections::BTreeMap<u32, Option<u64>>) {
        let renditions = {
            let sessions = self.shared.sessions.lock().await;
            let mut seen = std::collections::HashSet::new();
            sessions
                .values()
                .filter_map(|session| session.live_rendition())
                .filter(|rendition| seen.insert(rendition.key.clone()))
                .cloned()
                .collect::<Vec<_>>()
        };
        let mut reports = 0;
        let mut buckets = std::collections::BTreeMap::new();
        for rendition in renditions {
            if let Some(activity) = display_target_activity(&rendition) {
                if activity
                    .reported_peak_nits
                    .is_some_and(|nits| nits > 0 && nits < 5000)
                {
                    reports += 1;
                }
                if let Some(bucket) = activity.bucket_nits {
                    let bytes = rendition
                        .manifest
                        .try_lock()
                        .ok()
                        .map(|manifest| manifest.materialized_bytes());
                    let total = buckets.entry(bucket).or_insert(Some(0u64));
                    *total = total
                        .zip(bytes)
                        .map(|(total, bytes)| total.saturating_add(bytes));
                }
            }
        }
        (reports, buckets)
    }
}

#[cfg(test)]
mod cached_recovery_tests {
    #[tokio::test]
    async fn cached_reader_proof_requires_live_exact_incarnation_and_no_producer_demand() {
        let (_temp, serve, facts, _) =
            crate::vodserve::retained::test_post_attachment_output_facts().await;
        let id = "00000000-0000-4000-8000-00000000cace";
        let incarnation = "cached-recovery-incarnation";
        let output = facts
            .response_owner
            .retained_output_facts()
            .expect("actual immutable complete output");
        assert!(
            serve
                .cached_prepared_reader_owner(id, incarnation, &output)
                .await
                .is_none(),
            "ordinary cached publication is not a primed successor proof"
        );
        serve.mark_prepared_incarnation(id, incarnation).await;
        let owner = serve
            .cached_prepared_reader_owner(id, incarnation, &output)
            .await
            .expect("exact prepared retained reader");
        assert_eq!(
            serve.cached_prepared_reader_is_live(id, incarnation, &owner),
            Some(true)
        );
        assert_eq!(
            serve.cached_prepared_reader_is_live(id, "foreign-incarnation", &owner),
            Some(false)
        );
        {
            let _busy = owner.lifecycle.lock().await;
            assert!(
                serve
                    .cached_prepared_reader_is_live(id, incarnation, &owner)
                    .is_none(),
                "contended lifecycle cannot mint a survival claim"
            );
        }
        {
            let _busy = serve.shared.sessions.lock().await;
            assert_eq!(
                serve.cached_prepared_reader_is_live(id, incarnation, &owner),
                None
            );
        }
        crate::vodserve::retained::test_make_attachment_uncaptured(&serve, id).await;
        assert!(
            serve.cached_prepared_reader_is_live(id, incarnation, &owner) == Some(false),
            "cold attachment cannot retain an old cached proof"
        );
        assert!(serve
            .cached_prepared_reader_owner(id, incarnation, &output)
            .await
            .is_none());
    }
}

#[cfg(test)]
mod processing_activity_tests {
    use plurx_core::transcode::dv_processing::DvDestination;

    /// The Activity row of a live reconstruction names the delivered picture,
    /// the GPU work (compose + encode) and the reason it was chosen.
    #[test]
    fn reconstructed_profile81_activity_names_the_gpu_work_and_the_reason() {
        let row = super::processing_activity_text(
            DvDestination::Profile81,
            true,
            true,
            "hevc_vaapi",
            true,
        );
        assert_eq!(row.delivered, "Reconstructed Dolby Vision Profile 8.1");
        for part in [
            "VA-API decode",
            "base and full enhancement layers composed on the GPU",
            "hevc_vaapi Main10 encode",
            "rewritten 8.1 RPU",
        ] {
            assert!(row.work.contains(part), "{}", row.work);
        }
        assert!(row.reason.contains("Profile 7 FEL") && row.reason.contains("FEL re-encode is on"));
        assert!(row.fel_composed && row.live_rendering && row.bounded());
        let observation = crate::vodserve::VodActivityObservation {
            processing: Some(row.clone()),
            ..Default::default()
        };
        assert!(observation.bounded());
        // Peers carry it; an old peer's snapshot without it still decodes.
        let wire = serde_json::to_value(&observation).expect("observation wire");
        assert_eq!(wire["processing"]["fel_composed"], true);
        let old: crate::vodserve::VodActivityObservation =
            serde_json::from_value(serde_json::json!({"control_demand":"active"}))
                .expect("observation without processing");
        assert!(old.processing.is_none());
        let mut long = row;
        long.reason = "x".repeat(crate::vodserve::ProcessingActivity::MAX_TEXT + 1);
        assert!(!crate::vodserve::VodActivityObservation {
            processing: Some(long),
            ..Default::default()
        }
        .bounded());
        // Base-layer HDR10-E is not described as an FEL composition.
        let base =
            super::processing_activity_text(DvDestination::Hdr10, false, false, "libx265", true);
        assert!(!base.fel_composed && base.work.contains("software decode"));
        assert!(base.work.contains("base layer reshaped"), "{}", base.work);
    }
}
