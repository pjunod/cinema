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
        let sessions = self.shared.sessions.lock().await;
        let mut infos = sessions
            .iter()
            .filter(|(_, session)| session.tombstone.is_none())
            .filter_map(|(id, session)| {
                session.live_rendition()?;
                Some(VodDeliveryInfo {
                    id: id.clone(),
                    method: match &session.kind {
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
                })
            })
            .collect::<Vec<_>>();
        infos.sort_by(|left, right| {
            right
                .started_unix
                .cmp(&left.started_unix)
                .then(left.id.cmp(&right.id))
        });
        infos.truncate(limit);
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
        let mut readers = if segment_index.is_some() {
            Some(owner_rendition.readers.lock().await)
        } else {
            None
        };

        // Every await is above this line. Touch and frontier advance are one
        // cancellation-safe commit: EOF can never renew a session without
        // also recording the exact served frontier (or vice versa).
        *session.last_touch.lock().expect("touch lock") = Instant::now();
        if let (Some(index), Some(readers)) = (segment_index, readers.as_mut()) {
            if let Some(reader) = readers.get_mut(session_id) {
                reader.served(index);
            }
        }
        let marker_ledger = segment_index.and_then(|_| {
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
