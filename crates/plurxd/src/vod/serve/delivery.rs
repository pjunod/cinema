use super::*;

impl VodServe {
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
            .filter(|session| session.tombstone.is_none())
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
        if session.tombstone.is_some()
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
        session.tombstone.is_none()
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

    /// Frozen source facts carried by this exact VOD response owner. HTTP may
    /// prepare a representation from them before final owner admission without
    /// consulting whichever attachment currently reuses the public id.
    pub(crate) fn response_owner_file(&self, owner: &ResponseOwner) -> MediaFile {
        owner.file.as_ref().clone()
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

    /// Immutable playlist bytes: the same bytes for the session's whole life.
    pub async fn playlist(&self, session_id: &str) -> Option<VodPublication<Vec<u8>>> {
        let publication = self.session_rendition(session_id).await?;
        let (result, owner) = match publication.result {
            Ok((rendition, _, _)) => (Ok(rendition.playlist.clone()), publication.owner),
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
        Ok(bytes)
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
                            .any(|child| child._reservation.is_none())
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
                            Arc::ptr_eq(&child.rendition, rendition) && child._reservation.is_some()
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
                    .child_segment_before(session_id, &request, deadline)
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
                let interval = plurx_core::playback::continuous_quality::QualityInterval {
                    artifact_id: hex::encode(Sha256::digest(&media)),
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
                super::vod_serve_serve::verify_cached_quality_interval(
                    &init, &media, rung, &interval,
                )?;
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
                            .child_segment_before(session_id, &request, deadline)
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
                        let dependency =
                            plurx_core::playback::continuous_quality::QualityInterval {
                                artifact_id: hex::encode(Sha256::digest(&bytes)),
                                rendition_id: audio.rendition_id().into(),
                                timescale: soundtrack.rendition.timescale,
                                from_tick: entry.start_ticks,
                                through_tick: entry.end_ticks(),
                                byte_length: ready.len,
                            };
                        super::vod_serve_serve::verify_cached_shared_audio_interval(
                            &init,
                            &bytes,
                            audio,
                            &dependency,
                            index as usize + 1 == soundtrack.rendition.plan.len(),
                        )?;
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
        let budget = found
            .block_budget
            .min(deadline.saturating_duration_since(Instant::now()));
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
        if name == INIT_NAME {
            return Some(VodPublication {
                result: self
                    .serve_init(&rendition, budget, delivery)
                    .await
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
            result: self
                .serve_segment(&rendition, session_id, index, budget, delivery)
                .await
                .map(Some),
            owner,
        })
    }
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
