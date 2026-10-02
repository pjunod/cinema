use super::vod_serve_serve::QualityReservationCommit;
use super::*;
use plurx_core::playback::continuous_quality::{
    QualityAttachment, QualityLedger, QualityOperation, QualityPreparationBinding, QualityState,
    QualityTransitionReceipt, QualityTransitionRequest,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct QualityScheduleRequest {
    pub version: u8,
    pub generation: String,
    pub control_epoch: u64,
    pub attachment: QualityAttachment,
    pub transition: Option<QualityTransitionRequest>,
    pub frontier: Option<QualityAppendFrontier>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<QualityReadyWindow>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct QualityAppendFrontier {
    pub timescale: u32,
    pub through_tick: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct QualityReadyWindow {
    pub transaction_id: String,
    pub frontier: QualityAppendFrontier,
}
impl QualityAppendFrontier {
    fn valid(&self) -> bool {
        (1..=1_000_000).contains(&self.timescale) && self.through_tick <= 9_007_199_254_740_991
    }
}

impl QualityScheduleRequest {
    pub(crate) fn valid(&self) -> bool {
        self.version == 1
            && uuid::Uuid::parse_str(&self.generation).is_ok()
            && (1..=9_007_199_254_740_991).contains(&self.control_epoch)
            && self.attachment.valid()
            && self.transition.as_ref().is_none_or(|request| {
                request.valid()
                    && request.generation == self.generation
                    && request.control_epoch == self.control_epoch
                    && request.attachment == self.attachment
            })
            && match self.transition.as_ref().map(|request| &request.operation) {
                Some(QualityOperation::Prepare { .. }) => self
                    .frontier
                    .as_ref()
                    .is_some_and(|frontier| frontier.valid()),
                _ => self.frontier.is_none(),
            }
            && self.window.as_ref().is_none_or(|window| {
                self.transition.is_none()
                    && self.frontier.is_none()
                    && uuid::Uuid::parse_str(&window.transaction_id).is_ok()
                    && window.frontier.valid()
            })
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct QualityScheduleResponse {
    pub version: u8,
    pub generation: String,
    pub control_epoch: u64,
    pub attachment: QualityAttachment,
    pub revision: i64,
    /// A durable End fence permits reconciliation of a lost reservation ack.
    /// Omit on active replies to preserve the earlier additive v1 wire shape.
    #[serde(default, skip_serializing_if = "quality_flag_is_false")]
    pub terminal: bool,
    // A replay returns the original acknowledgement, separately from facts
    // that the owner has learned since that command was accepted.
    pub receipt: Option<QualityTransitionReceipt>,
    pub ledger: QualityLedger,
}
fn quality_flag_is_false(value: &bool) -> bool {
    !*value
}

impl QualityScheduleResponse {
    pub(crate) fn valid_for(&self, request: &QualityScheduleRequest) -> bool {
        self.version == 1
            && (!self.terminal
                || (request.window.is_none()
                    && request.frontier.is_none()
                    && request.transition.as_ref().is_none_or(|transition| {
                        !matches!(
                            transition.operation,
                            QualityOperation::Prepare { .. } | QualityOperation::Scheduled { .. }
                        )
                    })))
            && self.generation == request.generation
            && self.control_epoch == request.control_epoch
            && self.attachment == request.attachment
            && self.revision > 0
            && self.ledger.valid()
            && self.ledger.generation == self.generation
            && self.ledger.control_epoch == self.control_epoch
            && self.ledger.attachment == self.attachment
            && match (&self.receipt, &request.transition) {
                (Some(receipt), Some(transition)) => receipt.valid_for(transition),
                (None, None) => true,
                _ => false,
            }
    }
}

impl VodServe {
    pub(crate) async fn quality_schedule_before(
        &self,
        session_id: &str,
        owner_node_id: &str,
        request: &QualityScheduleRequest,
        deadline: Instant,
    ) -> Result<QualityScheduleResponse, String> {
        if !request.valid() {
            return Err("invalid quality schedule request".into());
        }
        let existing = self
            .shared
            .store
            .quality_ledger(&request.generation)
            .await
            .map_err(|error| error.to_string())?;
        let facts_only = request.window.is_none()
            && match request.transition.as_ref() {
                Some(transition) => !matches!(
                    transition.operation,
                    QualityOperation::Prepare { .. } | QualityOperation::Scheduled { .. }
                ),
                None => existing.is_some(),
            };
        let (owner, verified) = if facts_only {
            // Disposal and completed append facts remain reducible after a
            // producer/source failure. They introduce no physical reservation.
            let publication = self
                .session_rendition(session_id)
                .await
                .ok_or("quality parent is not attached")?;
            (publication.owner, None)
        } else {
            let publication = self
                .verified_continuous_family_before(session_id, deadline)
                .await
                .ok_or("continuous family is not attached")?;
            let verified = publication
                .result
                .map_err(|error| format!("{error:?}"))?
                .ok_or("continuous family is not verified")?;
            if verified.family.id() != request.attachment.family_id {
                return Err("quality family changed".into());
            }
            (publication.owner, Some(verified))
        };
        let family = verified.as_ref().map(|verified| &verified.family);
        if request
            .transition
            .as_ref()
            .is_some_and(|transition| match &transition.operation {
                QualityOperation::Prepare {
                    target_rendition_id,
                    ..
                } => family.is_none_or(|family| {
                    !family
                        .video()
                        .rungs()
                        .iter()
                        .any(|rung| rung.rendition_id() == target_rendition_id)
                }),
                _ => false,
            })
        {
            return Err("quality target is outside its family".into());
        }
        let mut receipt = None;
        let mut accepted = false;
        for _ in 0..4 {
            let snapshot = self
                .shared
                .store
                .quality_ledger(&request.generation)
                .await
                .map_err(|error| error.to_string())?;
            let mut candidate = if let Some(snapshot) = &snapshot {
                if snapshot.ledger.attachment != request.attachment {
                    return Err("quality attachment changed".into());
                }
                let mut ledger = snapshot.ledger.clone();
                if ledger.control_epoch < request.control_epoch {
                    ledger
                        .adopt_epoch(request.control_epoch)
                        .map_err(|error| error.to_string())?;
                }
                if ledger.control_epoch != request.control_epoch {
                    return Err("quality owner epoch changed".into());
                }
                ledger
            } else {
                if facts_only || request.window.is_some() {
                    return Err("quality facts have no attached ledger".into());
                }
                QualityLedger::new(
                    request.generation.clone(),
                    request.control_epoch,
                    request.attachment.clone(),
                )
                .map_err(|error| error.to_string())?
            };
            if let Some(transition) = &request.transition {
                receipt = Some(
                    candidate
                        .apply(transition, now_ms())
                        .map_err(|error| error.to_string())?,
                );
                if let Some(frontier) = &request.frontier {
                    candidate
                        .bind_preparation(
                            &transition.transaction_id,
                            QualityPreparationBinding {
                                timescale: frontier.timescale,
                                through_tick: frontier.through_tick,
                                deadline_ms: now_ms()
                                    .saturating_add(
                                        i64::try_from(
                                            deadline
                                                .saturating_duration_since(Instant::now())
                                                .as_millis(),
                                        )
                                        .unwrap_or(12_000),
                                    )
                                    .saturating_sub(3000),
                            },
                        )
                        .map_err(|error| error.to_string())?;
                }
                if let QualityOperation::CancelUnappended { .. } = transition.operation {
                    if candidate.transactions.iter().any(|tx| {
                        tx.transaction_id == transition.transaction_id
                            && tx.state == QualityState::Cancelling
                            && tx.reserved.is_empty()
                            && !tx.ever_appended
                    }) {
                        candidate
                            .cancelled(&transition.transaction_id)
                            .map_err(|error| error.to_string())?;
                    }
                }
            }
            let expired = candidate
                .transactions
                .iter()
                .filter(|tx| {
                    tx.state == QualityState::Preparing
                        && tx
                            .preparation
                            .as_ref()
                            .is_some_and(|binding| binding.deadline_ms <= now_ms())
                })
                .map(|tx| tx.transaction_id.clone())
                .collect::<Vec<_>>();
            for id in expired {
                candidate
                    .refuse_preparation(&id)
                    .map_err(|error| error.to_string())?;
            }
            if snapshot.as_ref().is_some_and(|old| old.ledger == candidate)
                || self
                    .publish_quality_candidate(QualityCandidatePublication {
                        session_id,
                        owner: &owner,
                        owner_node_id,
                        snapshot: snapshot.as_ref(),
                        candidate: &candidate,
                        family,
                        deadline,
                    })
                    .await?
            {
                accepted = true;
                break;
            }
        }
        if !accepted {
            return Err("quality schedule revision changed repeatedly".into());
        }
        if let (Some(transition), Some(_frontier), Some(family)) =
            (&request.transition, &request.frontier, family)
        {
            if let QualityOperation::Prepare {
                target_rendition_id,
                ..
            } = &transition.operation
            {
                let snapshot = self
                    .shared
                    .store
                    .quality_ledger(&request.generation)
                    .await
                    .map_err(|error| error.to_string())?
                    .ok_or("quality preparation ledger disappeared")?;
                if snapshot.ledger.transactions.iter().any(|tx| {
                    tx.transaction_id == transition.transaction_id
                        && tx.state == QualityState::Preparing
                        && !tx.cancel_requested
                        && !tx.intent_superseded
                }) {
                    let binding = snapshot
                        .ledger
                        .transactions
                        .iter()
                        .find(|tx| tx.transaction_id == transition.transaction_id)
                        .and_then(|tx| tx.preparation.as_ref())
                        .ok_or("quality preparation frontier disappeared")?;
                    let remaining = binding.deadline_ms.saturating_sub(now_ms());
                    let preparation_deadline =
                        Instant::now() + Duration::from_millis(remaining.max(0) as u64);
                    let preparation_deadline = preparation_deadline.min(deadline);
                    let ready = self
                        .quality_ready_before(
                            session_id,
                            family,
                            target_rendition_id,
                            binding.timescale,
                            binding.through_tick,
                            preparation_deadline,
                        )
                        .await;
                    for _ in 0..4 {
                        let snapshot = self
                            .shared
                            .store
                            .quality_ledger(&request.generation)
                            .await
                            .map_err(|error| error.to_string())?
                            .ok_or("quality preparation ledger disappeared")?;
                        if snapshot.ledger.attachment != request.attachment
                            || snapshot.ledger.control_epoch != request.control_epoch
                        {
                            return Err("quality preparation owner changed".into());
                        }
                        let mut candidate = snapshot.ledger.clone();
                        let Some(tx) = candidate
                            .transactions
                            .iter()
                            .find(|tx| tx.transaction_id == transition.transaction_id)
                        else {
                            break;
                        };
                        if tx.state != QualityState::Preparing
                            || tx.cancel_requested
                            || tx.intent_superseded
                        {
                            break;
                        }
                        match &ready {
                            Ok(intervals) => candidate
                                .ready(&transition.transaction_id, intervals.clone())
                                .map_err(|error| error.to_string())?,
                            Err(_) => candidate
                                .refuse_preparation(&transition.transaction_id)
                                .map_err(|error| error.to_string())?,
                        }
                        if self
                            .publish_quality_candidate(QualityCandidatePublication {
                                session_id,
                                owner: &owner,
                                owner_node_id,
                                snapshot: Some(&snapshot),
                                candidate: &candidate,
                                family: Some(family),
                                deadline,
                            })
                            .await?
                        {
                            break;
                        }
                    }
                }
            }
        }
        if let Some(window) = &request.window {
            let family = family.ok_or("quality window has no verified family")?;
            let snapshot = self
                .shared
                .store
                .quality_ledger(&request.generation)
                .await
                .map_err(|error| error.to_string())?
                .ok_or("quality window ledger disappeared")?;
            let tx = snapshot
                .ledger
                .transactions
                .iter()
                .find(|tx| tx.transaction_id == window.transaction_id)
                .ok_or("quality window transaction disappeared")?;
            if tx.cancel_requested
                || tx.intent_superseded
                || !matches!(
                    tx.state,
                    QualityState::Ready
                        | QualityState::Scheduled
                        | QualityState::Appended
                        | QualityState::Presented
                        | QualityState::Disposed
                )
            {
                return Err("quality window target is no longer wanted".into());
            }
            let target = tx.target_rendition_id.clone();
            let preparation_deadline = deadline
                .checked_sub(Duration::from_secs(3))
                .unwrap_or(deadline);
            let intervals = self
                .quality_ready_before(
                    session_id,
                    family,
                    &target,
                    window.frontier.timescale,
                    window.frontier.through_tick,
                    preparation_deadline,
                )
                .await?;
            let mut settled = false;
            for _ in 0..4 {
                let snapshot = self
                    .shared
                    .store
                    .quality_ledger(&request.generation)
                    .await
                    .map_err(|error| error.to_string())?
                    .ok_or("quality window ledger disappeared")?;
                if snapshot.ledger.attachment != request.attachment
                    || snapshot.ledger.control_epoch != request.control_epoch
                {
                    return Err("quality window owner changed".into());
                }
                let mut candidate = snapshot.ledger.clone();
                candidate
                    .ready(&window.transaction_id, intervals.clone())
                    .map_err(|error| error.to_string())?;
                if self
                    .publish_quality_candidate(QualityCandidatePublication {
                        session_id,
                        owner: &owner,
                        owner_node_id,
                        snapshot: Some(&snapshot),
                        candidate: &candidate,
                        family: Some(family),
                        deadline,
                    })
                    .await?
                {
                    settled = true;
                    break;
                }
            }
            if !settled {
                return Err("quality window revision changed repeatedly".into());
            }
        }
        let snapshot = self
            .shared
            .store
            .quality_ledger(&request.generation)
            .await
            .map_err(|error| error.to_string())?
            .ok_or("quality ledger disappeared")?;
        if snapshot.ledger.attachment != request.attachment
            || snapshot.ledger.control_epoch != request.control_epoch
            || !self.response_owner_is_live(session_id, &owner).await
        {
            return Err("quality response owner changed".into());
        }
        Ok(QualityScheduleResponse {
            version: 1,
            generation: request.generation.clone(),
            control_epoch: request.control_epoch,
            attachment: request.attachment.clone(),
            revision: snapshot.revision,
            terminal: false,
            receipt,
            ledger: snapshot.ledger,
        })
    }
}

struct QualityCandidatePublication<'a> {
    session_id: &'a str,
    owner: &'a ResponseOwner,
    owner_node_id: &'a str,
    snapshot: Option<&'a plurx_core::store::QualityLedgerSnapshot>,
    candidate: &'a QualityLedger,
    family: Option<&'a plurx_core::transcode::VodPresentationFamily>,
    deadline: Instant,
}
impl VodServe {
    async fn publish_quality_candidate(
        &self,
        publication: QualityCandidatePublication<'_>,
    ) -> Result<bool, String> {
        let QualityCandidatePublication {
            session_id,
            owner,
            owner_node_id,
            snapshot,
            candidate,
            family,
            deadline,
        } = publication;
        if let Some(expected) = snapshot {
            let old = expected
                .ledger
                .transactions
                .iter()
                .flat_map(|tx| &tx.reserved)
                .collect::<Vec<_>>();
            let adds_pins = candidate
                .transactions
                .iter()
                .flat_map(|tx| &tx.reserved)
                .any(|interval| !old.contains(&interval))
                || candidate
                    .shared_audio_reserved()
                    .iter()
                    .any(|interval| !expected.ledger.shared_audio_reserved().contains(interval));
            if adds_pins {
                let family = family.ok_or("quality reservation has no verified family")?;
                return self
                    .commit_quality_reservations_bound(
                        QualityReservationCommit {
                            owner_node_id,
                            expected,
                            candidate,
                            family,
                            now_ms: now_ms(),
                            deadline,
                        },
                        Some((session_id, owner)),
                    )
                    .await;
            }
        }
        // Initial and facts-only writes introduce no physical pins. Bind it to the same attachment
        // lifetime gate that serializes local End and reattachment, and retain
        // that gate until a submitted Store CAS settles after cancellation.
        let guard = Arc::clone(&owner.lifecycle).lock_owned().await;
        {
            let sessions = self.shared.sessions.lock().await;
            if sessions.get(session_id).is_none_or(|session| {
                session.tombstone.is_some()
                    || !Arc::ptr_eq(&session.incarnation, &owner.incarnation)
                    || !session.owns_response_media(owner)
            }) {
                return Err("quality ledger parent attachment changed".into());
            }
        }
        let store = Arc::clone(&self.shared.store);
        let revision = snapshot.map_or(0, |snapshot| snapshot.revision);
        let candidate = candidate.clone();
        let owner_node_id = owner_node_id.to_owned();
        let settlement = tokio::spawn(async move {
            let result = store
                .write_quality_ledger(&candidate, &owner_node_id, revision, now_ms())
                .await
                .map_err(|error| error.to_string());
            drop(guard);
            result
        });
        tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), settlement)
            .await
            .map_err(|_| "quality ledger settlement exceeded its inherited deadline".to_owned())?
            .map_err(|error| error.to_string())?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn schedule_identity_and_frontier_are_strict_and_independently_versioned() {
        let attachment = QualityAttachment {
            client_instance_id: uuid::Uuid::new_v4().to_string(),
            lifetime_id: "movie".into(),
            attachment_id: uuid::Uuid::new_v4().to_string(),
            family_id: "a".repeat(64),
        };
        let generation = uuid::Uuid::new_v4().to_string();
        let mut request = QualityScheduleRequest {
            version: 1,
            generation: generation.clone(),
            control_epoch: 1,
            attachment: attachment.clone(),
            frontier: None,
            transition: None,
            window: None,
        };
        assert!(request.valid());
        request.transition = Some(QualityTransitionRequest {
            version: 1,
            generation,
            control_epoch: 1,
            sequence: 1,
            attachment,
            transaction_id: uuid::Uuid::new_v4().to_string(),
            operation: QualityOperation::Prepare {
                intent_revision: 1,
                target_rendition_id: "b".repeat(64),
            },
        });
        assert!(
            !request.valid(),
            "prepare requires an exact append frontier"
        );
        request.frontier = Some(QualityAppendFrontier {
            timescale: 24000,
            through_tick: 240240,
        });
        assert!(request.valid());
        let mut json = serde_json::to_value(&request).expect("wire");
        json["extra"] = serde_json::json!(true);
        assert!(serde_json::from_value::<QualityScheduleRequest>(json).is_err());
        request
            .transition
            .as_mut()
            .expect("transition")
            .control_epoch = 2;
        assert!(!request.valid(), "nested owner cannot differ");
    }
}
