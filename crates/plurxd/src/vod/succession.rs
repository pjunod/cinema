//! A finite rendition's successor generation (A-C6EECF11).
//!
//! A finite producer encodes one suffix of the title, from its start entry.
//! When readers need an entry it cannot reach (before its start, or further
//! ahead than a restart costs, by the legacy scheduler's own rule), a new
//! Rendition incarnation replaces it at that entry. Every session attached to
//! the predecessor is parked first: its reader and finite client slots are
//! released so the predecessor can drain, and its GETs answer Pending. The
//! predecessor then closes through the ordinary finite close, which hands its
//! charge, compatibility share and cache grant to the successor at its locked
//! refund. The successor is built with that handover and every parked session
//! is reattached with its reader state, re-reserving its slots from the
//! successor's own lease. A refused rebind leaves each parked session a typed
//! capacity answer. There is no timer: each step waits on its real owner.
use super::*;
use plurx_core::adaptive_resources::runtime::finite_vod::{
    FiniteSuccessorDemand, FiniteSuccessorGrant,
};

/// A successor waiting for its predecessor's finite close to hand over.
pub(super) struct PendingSuccessor {
    pub(super) demand: FiniteSuccessorDemand,
    pub(super) reply: tokio::sync::oneshot::Sender<Result<SuccessorHandover, String>>,
}

/// What a predecessor's close hands to its successor generation.
pub(crate) struct SuccessorHandover {
    pub(super) grant: FiniteSuccessorGrant,
    pub(super) compatibility: Option<crate::vodencode::FiniteSoftwareCompatibility>,
    pub(super) cache_grant: Option<crate::playback_cache::FiniteCacheReservation>,
}

impl std::fmt::Debug for SuccessorHandover {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SuccessorHandover").finish_non_exhaustive()
    }
}

/// A session whose attachment to a rendition is between incarnations.
#[derive(Debug, Default)]
pub(super) struct SuccessionState {
    /// Rendition keys this session is parked on.
    pub(super) keys: Vec<String>,
    /// The typed refusal its successor received, once refused.
    pub(super) refused: Option<String>,
}

impl SuccessionState {
    pub(super) fn requested_heap_bytes(&self) -> Option<usize> {
        let mut bytes = self
            .keys
            .capacity()
            .checked_mul(std::mem::size_of::<String>())?;
        for key in &self.keys {
            bytes = bytes.checked_add(key.capacity())?;
        }
        bytes.checked_add(self.refused.as_ref().map_or(0, String::capacity))
    }
}

/// The reader facts a parked attachment keeps across incarnations.
struct ParkedReaderState {
    frontier: u32,
    control_sequence: Option<u64>,
    admission_priority: crate::admission::Priority,
    companion_bootstrap: bool,
    control_required: bool,
    quality_required: bool,
    authority_only: bool,
    nonpreemptive_trial: bool,
    preparation_frontier: Option<u32>,
    last_served: Option<u32>,
}

impl ParkedReaderState {
    fn of(reader: &Reader) -> Self {
        Self {
            frontier: reader.frontier,
            control_sequence: reader.control_sequence,
            admission_priority: reader.admission_priority,
            companion_bootstrap: reader.companion_bootstrap,
            control_required: reader.control_required,
            quality_required: reader.quality_required,
            authority_only: reader.authority_only,
            nonpreemptive_trial: reader.nonpreemptive_trial,
            preparation_frontier: reader.preparation_frontier,
            last_served: reader.last_served,
        }
    }

    fn apply(&self, reader: &mut Reader) {
        reader.frontier = self.frontier;
        reader.control_sequence = self.control_sequence;
        reader.admission_priority = self.admission_priority;
        reader.companion_bootstrap = self.companion_bootstrap;
        reader.control_required = self.control_required;
        reader.quality_required = self.quality_required;
        reader.authority_only = self.authority_only;
        reader.nonpreemptive_trial = self.nonpreemptive_trial;
        reader.preparation_frontier = self.preparation_frontier;
        reader.last_served = self.last_served;
    }
}

enum ParkedRole {
    Primary {
        reader: Option<ParkedReaderState>,
        control_slot: bool,
    },
    Child {
        versioned_family: bool,
        companion_bootstrap: bool,
        controlled: bool,
        candidate_id: Option<plurx_core::playback::candidate::CandidateId>,
        reader_id: String,
        reservation: Option<crate::vodencode::EncodePermit>,
        reader: Option<ParkedReaderState>,
    },
}

struct ParkedAttachment {
    session_id: String,
    lifecycle: Arc<Mutex<()>>,
    incarnation: Arc<()>,
    role: ParkedRole,
}

impl Shared {
    /// Abandon every pending successor (its collector stopped). A pending
    /// successor holds a demand and a reply, never a grant, so nothing is
    /// orphaned; each waiting succession refuses its parked sessions.
    pub(super) fn drop_pending_successors(&self) {
        self.pending_successors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }

    /// The successor a close of `key` must hand its charge to, if any.
    pub(super) fn take_pending_successor(&self, key: &str) -> Option<PendingSuccessor> {
        self.pending_successors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(key)
    }

    /// Begin replacing `old` with a generation that starts at `entry`,
    /// unless a succession of this key is already in flight.
    pub(super) fn request_finite_succession(self: &Arc<Self>, old: &Arc<Rendition>, entry: u32) {
        let key = old.key.clone();
        if !self
            .successions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key.clone())
        {
            return;
        }
        let shared = Arc::clone(self);
        let old = Arc::downgrade(old);
        tokio::spawn(async move {
            if let Err(error) = shared.succeed_finite(old, entry).await {
                tracing::warn!(target: "plurxd::vodserve", rendition = %key, %error,
                    "finite successor generation did not install");
            }
            shared
                .successions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&key);
        });
    }

    async fn succeed_finite(
        self: &Arc<Self>,
        old: std::sync::Weak<Rendition>,
        entry: u32,
    ) -> Result<(), String> {
        let Some(old) = old.upgrade() else {
            return Ok(());
        };
        let key = old.key.clone();
        if old.closed.load(Acquire) || old.failure().is_some() {
            return Ok(());
        }
        let settings = old
            .build_settings
            .clone()
            .ok_or("finite rendition has no build settings for a successor")?;
        let producer = old
            .dir
            .finite_producer()
            .ok_or("finite rendition has no producer to succeed")?;
        let demand = producer.successor_demand().await?;
        drop(producer);
        let start = super::plan::video_entry_at_or_before(&old.plan, entry);
        let start_seconds = old
            .plan
            .entry(start)
            .map(|planned| planned.start_ticks as f64 / f64::from(old.plan.timescale.max(1)))
            .ok_or("successor entry is outside the plan")?;
        let recipe = old.recipe.clone();
        let plan = old.plan.clone();
        let index = old.index.clone();

        // 1. Park every attachment, so the predecessor can drain.
        let parked = self.park_for_succession(&old).await;
        // 2. Register the successor, then retire the predecessor exactly.
        // Only a weak reference survives this point: the close waits for
        // every application alias, and without a collector it runs inline.
        let (reply, handover) = tokio::sync::oneshot::channel();
        self.pending_successors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key.clone(), PendingSuccessor { demand, reply });
        let original = Arc::downgrade(&old);
        drop(old);
        self.purge_dormant_inner(&key, Duration::ZERO, false, Some(original.clone()))
            .await;
        let closing = self.finite_closing.lock().await.contains_key(&key);
        let unclosed = original
            .upgrade()
            .filter(|old| !old.closed.load(Acquire) && !closing);
        if let Some(old) = unclosed {
            // The exact predecessor did not enter its close (a new reader
            // attached, or it is still protected). Nothing was handed over:
            // put every parked attachment back on the same incarnation.
            self.take_pending_successor(&key);
            self.reattach(&old, parked).await;
            return Err("the predecessor did not close for its successor".into());
        }
        // 3. The close owner answers once its locked refund ran.
        let handover = match handover.await {
            Ok(Ok(handover)) => handover,
            Ok(Err(refusal)) => {
                self.refuse_parked(&key, parked, refusal.clone()).await;
                return Err(refusal);
            }
            Err(_) => {
                let refusal = "the predecessor close ended without a handover".to_owned();
                self.refuse_parked(&key, parked, refusal.clone()).await;
                return Err(refusal);
            }
        };
        // 4. Build and publish the successor with that handover.
        let admission = crate::vodencode::FiniteAdmissionRequest {
            priority: crate::admission::Priority::Live,
            prepared: None,
            start_seconds,
            successor: Some(Arc::new(std::sync::Mutex::new(Some(handover)))),
        };
        let built = async {
            let gate = self.rendition_build_gate(&key)?.lock_owned().await;
            let rendition = self
                .build_rendition_with_storage(
                    &key,
                    index,
                    recipe,
                    plan,
                    &settings,
                    None,
                    false,
                    None,
                    super::shared::AttachmentPurpose::Playback,
                    &admission,
                )
                .await?;
            self.publish_built_rendition(&key, rendition, gate).await
        }
        .await;
        match built {
            Ok(attachment) => {
                let rendition = Arc::clone(&attachment.rendition);
                drop(attachment);
                self.reattach(&rendition, parked).await;
                Ok(())
            }
            Err(error) => {
                self.refuse_parked(&key, parked, error.clone()).await;
                Err(error)
            }
        }
    }

    /// Detach every session attachment of `old`, keeping its reader facts.
    async fn park_for_succession(self: &Arc<Self>, old: &Arc<Rendition>) -> Vec<ParkedAttachment> {
        let candidates = {
            let sessions = self.sessions.lock().await;
            sessions
                .iter()
                .filter(|(_, session)| {
                    session.tombstone.is_none()
                        && (session
                            .rendition
                            .as_ref()
                            .is_some_and(|current| Arc::ptr_eq(current, old))
                            || session
                                .children
                                .iter()
                                .any(|child| Arc::ptr_eq(&child.rendition, old)))
                })
                .map(|(id, session)| {
                    (
                        id.clone(),
                        Arc::clone(&session.lifecycle),
                        Arc::clone(&session.incarnation),
                    )
                })
                .collect::<Vec<_>>()
        };
        let mut parked = Vec::new();
        for (session_id, lifecycle, incarnation) in candidates {
            let _lifecycle = lifecycle.lock().await;
            let mut sessions = self.sessions.lock().await;
            let Some(session) = sessions.get_mut(&session_id) else {
                continue;
            };
            if session.tombstone.is_some() || !Arc::ptr_eq(&session.incarnation, &incarnation) {
                continue;
            }
            let mut readers = old.readers.lock().await;
            let mut retired = Vec::new();
            if session
                .rendition
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, old))
            {
                let reader = readers.remove(&session_id);
                retired.push(session_id.clone());
                let control_slot = session.finite_control_slot.is_some();
                session.finite_session_slot = None;
                session.finite_control_slot = None;
                session.rendition = None;
                session
                    .succession
                    .get_or_insert_with(Default::default)
                    .keys
                    .push(old.key.clone());
                parked.push(ParkedAttachment {
                    session_id: session_id.clone(),
                    lifecycle: Arc::clone(&lifecycle),
                    incarnation: Arc::clone(&incarnation),
                    role: ParkedRole::Primary {
                        reader: reader.as_ref().map(ParkedReaderState::of),
                        control_slot,
                    },
                });
            }
            while let Some(index) = session
                .children
                .iter()
                .position(|child| Arc::ptr_eq(&child.rendition, old))
            {
                let child = session.children.remove(index);
                let reader = readers.remove(&child.reader_id);
                retired.push(child.reader_id.clone());
                session
                    .succession
                    .get_or_insert_with(Default::default)
                    .keys
                    .push(old.key.clone());
                parked.push(ParkedAttachment {
                    session_id: session_id.clone(),
                    lifecycle: Arc::clone(&lifecycle),
                    incarnation: Arc::clone(&incarnation),
                    role: ParkedRole::Child {
                        versioned_family: child.versioned_family,
                        companion_bootstrap: child.companion_bootstrap,
                        controlled: child.controlled,
                        candidate_id: child.candidate_id,
                        reader_id: child.reader_id.clone(),
                        reservation: child._reservation,
                        reader: reader.as_ref().map(ParkedReaderState::of),
                    },
                });
            }
            if readers.is_empty() {
                *old.dormant_since.lock().expect("dormant lock") = Some(Instant::now());
            }
            drop(readers);
            // Blocked GETs of these readers wake now and re-resolve through
            // the parked session, which answers Pending.
            for reader_id in retired {
                self.pool.retire_session(&old.key, &reader_id);
            }
        }
        old.kick();
        parked
    }

    /// Attach every parked attachment to `rendition`, re-reserving each
    /// session's finite slots and reader from that incarnation's lease. An
    /// attachment whose slots no longer fit gets the typed capacity answer.
    async fn reattach(self: &Arc<Self>, rendition: &Arc<Rendition>, parked: Vec<ParkedAttachment>) {
        for attachment in parked {
            let _lifecycle = attachment.lifecycle.lock().await;
            let mut sessions = self.sessions.lock().await;
            let Some(session) = sessions.get_mut(&attachment.session_id) else {
                continue;
            };
            if session.tombstone.is_some()
                || !Arc::ptr_eq(&session.incarnation, &attachment.incarnation)
            {
                continue;
            }
            let mut readers = rendition.readers.lock().await;
            let attached = match attachment.role {
                ParkedRole::Primary {
                    reader,
                    control_slot,
                } => {
                    session.rendition = Some(Arc::clone(rendition));
                    let reserved = session
                        .reserve_finite_session_state(&attachment.session_id)
                        .and_then(|()| {
                            if control_slot {
                                session.reserve_finite_control_growth(0)
                            } else {
                                Ok(())
                            }
                        })
                        .and_then(|()| {
                            fresh_reader(rendition, &attachment.session_id, reader.as_ref(), true)
                        })
                        .and_then(|(key, fresh)| {
                            readers
                                .try_insert(key, fresh)
                                .map(drop)
                                .map_err(|_| "finite reader table has no remaining slot".to_owned())
                        });
                    if reserved.is_err() {
                        session.rendition = None;
                        session.finite_session_slot = None;
                        session.finite_control_slot = None;
                    }
                    reserved
                }
                ParkedRole::Child {
                    versioned_family,
                    companion_bootstrap,
                    controlled,
                    candidate_id,
                    reader_id,
                    reservation,
                    reader,
                } => fresh_reader(rendition, &reader_id, reader.as_ref(), false)
                    .and_then(|(key, fresh)| {
                        readers
                            .try_insert(key, fresh)
                            .map(drop)
                            .map_err(|_| "finite reader table has no remaining slot".to_owned())
                    })
                    .map(|()| {
                        session.children.push(ParentMediaReader {
                            versioned_family,
                            companion_bootstrap,
                            controlled,
                            candidate_id,
                            reader_id,
                            rendition: Arc::clone(rendition),
                            _reservation: reservation,
                        });
                    }),
            };
            drop(readers);
            match attached {
                Ok(()) => {
                    if let Some(state) = session.succession.as_mut() {
                        if let Some(at) = state.keys.iter().position(|key| *key == rendition.key) {
                            state.keys.remove(at);
                        }
                        if state.keys.is_empty() && state.refused.is_none() {
                            session.succession = None;
                        }
                    }
                    *rendition.dormant_since.lock().expect("dormant lock") = None;
                }
                Err(error) => {
                    session
                        .succession
                        .get_or_insert_with(Default::default)
                        .refused = Some(error);
                }
            }
        }
        rendition.kick();
    }

    /// The successor was refused: every parked attachment keeps its key and
    /// answers the typed capacity refusal until its session ends.
    async fn refuse_parked(
        self: &Arc<Self>,
        key: &str,
        parked: Vec<ParkedAttachment>,
        refusal: String,
    ) {
        for attachment in parked {
            let _lifecycle = attachment.lifecycle.lock().await;
            let mut sessions = self.sessions.lock().await;
            let Some(session) = sessions.get_mut(&attachment.session_id) else {
                continue;
            };
            if session.tombstone.is_some()
                || !Arc::ptr_eq(&session.incarnation, &attachment.incarnation)
            {
                continue;
            }
            let state = session.succession.get_or_insert_with(Default::default);
            if !state.keys.iter().any(|parked| parked == key) {
                state.keys.push(key.to_owned());
            }
            state.refused = Some(refusal.clone());
        }
    }
}

/// A reader for `key` on `rendition`'s own lease, carrying its parked facts.
fn fresh_reader(
    rendition: &Arc<Rendition>,
    key: &str,
    parked: Option<&ParkedReaderState>,
    primary: bool,
) -> Result<(String, Reader), String> {
    let priority = parked.map_or(
        if primary {
            crate::admission::Priority::Live
        } else {
            crate::admission::Priority::Speculative
        },
        |state| state.admission_priority,
    );
    let frontier = parked.map_or(0, |state| state.frontier);
    let (key, mut reader) = rendition.prepare_client_reader(key, frontier, priority)?;
    if let Some(state) = parked {
        state.apply(&mut reader);
    }
    Ok((key, reader))
}

/// The predecessor's close outcome for its pending successor, if any.
pub(super) type SuccessorOutcome = (
    Result<FiniteSuccessorGrant, plurx_core::adaptive_resources::AdmissionRefusal>,
    tokio::sync::oneshot::Sender<Result<SuccessorHandover, String>>,
);

/// After a finite close's locked refund: hand the compatibility share and
/// cache grant to a granted successor, or retire them exactly as an
/// ordinary close does. A successor that stopped waiting gets nothing; its
/// grant drops as a clean refund and the parts retire.
pub(super) fn settle_successor_handover(
    compatibility: Option<crate::vodencode::FiniteSoftwareCompatibility>,
    cache_grant: Option<crate::playback_cache::FiniteCacheReservation>,
    outcome: Option<SuccessorOutcome>,
) {
    let retire =
        |compatibility: Option<crate::vodencode::FiniteSoftwareCompatibility>,
         cache_grant: Option<crate::playback_cache::FiniteCacheReservation>| {
            if let Some(compatibility) = compatibility {
                compatibility.known_retired();
            }
            if let Some(cache_grant) = cache_grant {
                cache_grant.known_closed();
            }
        };
    match outcome {
        Some((Ok(grant), reply)) => {
            if let Err(Ok(returned)) = reply.send(Ok(SuccessorHandover {
                grant,
                compatibility,
                cache_grant,
            })) {
                retire(returned.compatibility, returned.cache_grant);
            }
        }
        Some((Err(refusal), reply)) => {
            retire(compatibility, cache_grant);
            let _ = reply.send(Err(format!(
                "finite successor admission refused: {refusal:?}"
            )));
        }
        None => retire(compatibility, cache_grant),
    }
}
