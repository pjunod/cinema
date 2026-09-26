use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct MarkerDestination {
    pub(super) kind: AnnotationKind,
    pub(super) start_ms: i64,
    pub(super) end_ms: i64,
    pub(super) target_entry: u32,
    pub(super) window_end_entry: u32,
    pub(super) eligible: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PrewarmedRange {
    pub(super) first: u32,
    pub(super) last: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PrewarmedEntry {
    index: u32,
    publication: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MarkerPrewarmRecord {
    pub(super) destination: MarkerDestination,
    pub(super) requested_sequence: u64,
    /// Ledger-local identity. Unlike the protocol sequence, this never resets
    /// when control ownership changes, so an old dispatch cannot credit a
    /// replacement request that happens to reuse the same sequence number.
    pub(super) nonce: u64,
    /// True only while the latest accepted snapshot is inside this marker's
    /// approach window. Historical provenance stays in the row after this is
    /// cleared, but it can no longer schedule work.
    pub(super) schedulable: bool,
    pub(super) active: bool,
    pub(super) settled: bool,
    pub(super) produced: Vec<PrewarmedEntry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct MarkerPrewarmRequest {
    destination: MarkerDestination,
    pub(super) nonce: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MarkerPendingSkip {
    request: MarkerPrewarmRequest,
    /// Exact prewarm publication present when the skip beacon was consumed.
    /// `None` is a durable miss: later production cannot upgrade it.
    publication_at_skip: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct MarkerAwaitingBeacon {
    request: MarkerPrewarmRequest,
    /// Exact publication observed at a post-marker Rendering snapshot. This
    /// lets a delayed beacon settle without treating old forward-buffer fetches
    /// as landing evidence.
    publication_at_landing: Option<u64>,
}

impl MarkerPrewarmRecord {
    fn credited(&self, entry: u32) -> bool {
        self.produced.iter().any(|produced| produced.index == entry)
    }

    fn credit(&mut self, entry: u32, publication: u64) {
        if entry > self.destination.window_end_entry {
            return;
        }
        if let Some(existing) = self
            .produced
            .iter_mut()
            .find(|produced| produced.index == entry)
        {
            existing.publication = publication;
            return;
        }
        self.produced.push(PrewarmedEntry {
            index: entry,
            publication,
        });
    }

    pub(super) fn credited_publication(&self, entry: u32, publication: Option<u64>) -> bool {
        publication.is_some_and(|publication| {
            self.produced
                .iter()
                .any(|produced| produced.index == entry && produced.publication == publication)
        })
    }
}

/// Bounded provenance ledger for one playback. There can be at most one row
/// per stored annotation (the store itself caps that set), and rows survive
/// settlement so a later marker seek can prove who produced its landing
/// segment rather than consulting the ordinary forward buffer.
#[derive(Debug, Default)]
pub(super) struct MarkerPrewarmLedger {
    pub(super) enabled: bool,
    pub(super) records: Vec<MarkerPrewarmRecord>,
    next_nonce: u64,
    /// A client marker beacon is the explicit skip intent. Control snapshots
    /// arm its exact stored destination while playback is inside the marker;
    /// the beacon moves it here until a served segment or later snapshot
    /// proves the landing.
    pub(super) approach_skip: Option<MarkerPrewarmRequest>,
    pub(super) armed_skip: Option<MarkerPrewarmRequest>,
    pending_skip: Option<MarkerPendingSkip>,
    pub(super) awaiting_beacon: Option<MarkerAwaitingBeacon>,
    last_settled_skip: Option<MarkerPrewarmRequest>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct MarkerPrewarmCandidate {
    pub(super) target_entry: u32,
    pub(super) window_end_entry: u32,
    pub(super) target_materialized: bool,
}

pub(super) struct MarkerPrewarmDecision {
    pub(super) action: Action,
    pub(super) candidate: Option<MarkerPrewarmCandidate>,
    pub(super) owners: Vec<MarkerPrewarmOwner>,
}

#[derive(Clone)]
pub(super) struct MarkerPrewarmOwner {
    pub(super) ledger: Arc<StdMutex<MarkerPrewarmLedger>>,
    pub(super) record_nonce: u64,
}

pub(super) struct MarkerPrewarmDispatch {
    pub(super) producer_epoch: u64,
    pub(super) candidate: MarkerPrewarmCandidate,
    pub(super) owners: Vec<MarkerPrewarmOwner>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct MarkerPrewarmOutcome {
    pub(super) hit: bool,
    pub(super) destination: MarkerDestination,
    pub(super) requested_sequence: Option<u64>,
    pub(super) produced_range: Option<PrewarmedRange>,
}

#[derive(Debug)]
pub(super) struct MarkerClientSkipResult {
    pub(super) matched: bool,
    pub(super) outcome: Option<MarkerPrewarmOutcome>,
}

pub(super) struct MarkerPrewarmControl {
    pub(super) rendition: Arc<Rendition>,
    pub(super) snapshot: crate::playback_control::PlaybackDemandSnapshot,
    pub(super) destinations: Vec<MarkerDestination>,
    pub(super) sequence: u64,
    pub(super) file_id: i64,
    pub(super) kind: SessionKind,
}

impl MarkerPrewarmLedger {
    fn allocate_nonce(&mut self) -> u64 {
        self.next_nonce = self.next_nonce.saturating_add(1);
        self.next_nonce
    }

    fn request_for(&self, destination: MarkerDestination) -> Option<MarkerPrewarmRequest> {
        self.records
            .iter()
            .find(|record| record.destination == destination && !record.settled)
            .map(|record| MarkerPrewarmRequest {
                destination,
                nonce: record.nonce,
            })
    }

    pub(super) fn update_control(
        &mut self,
        sequence: u64,
        snapshot: &crate::playback_control::PlaybackDemandSnapshot,
        destinations: &[MarkerDestination],
    ) {
        for record in &mut self.records {
            record.schedulable = false;
        }
        let scheduling_enabled = snapshot.demand == crate::playback_control::PlaybackDemand::Active
            && snapshot.render_state != crate::playback_control::RenderState::Seeking
            && snapshot.playback_rate > 0.0;
        self.enabled = scheduling_enabled;

        let approach_ms = (MARKER_PREWARM_APPROACH_WALL_MS as f64 * snapshot.playback_rate)
            .round()
            .clamp(0.0, i64::MAX as f64) as i64;
        let approach_end_ms = snapshot.position_ms.saturating_add(approach_ms);
        let approaching = destinations
            .iter()
            .copied()
            .filter(|destination| {
                destination.eligible
                    && snapshot.position_ms < destination.end_ms
                    && destination.start_ms <= approach_end_ms
            })
            .collect::<Vec<_>>();
        for destination in &approaching {
            let existing = self
                .records
                .iter()
                .position(|record| record.destination == *destination);
            match existing {
                Some(index) if self.records[index].settled => {
                    let nonce = self.allocate_nonce();
                    self.records[index] = MarkerPrewarmRecord {
                        destination: *destination,
                        requested_sequence: sequence,
                        nonce,
                        schedulable: scheduling_enabled,
                        active: false,
                        settled: false,
                        produced: Vec::new(),
                    };
                    self.last_settled_skip = None;
                }
                Some(index) => self.records[index].schedulable = scheduling_enabled,
                None => {
                    let nonce = self.allocate_nonce();
                    self.records.push(MarkerPrewarmRecord {
                        destination: *destination,
                        requested_sequence: sequence,
                        nonce,
                        schedulable: scheduling_enabled,
                        active: false,
                        settled: false,
                        produced: Vec::new(),
                    });
                }
            }
        }
        let mut approach_requests = approaching
            .iter()
            .filter_map(|destination| self.request_for(*destination));
        let approach_request = approach_requests.next();
        self.approach_skip = approach_requests
            .next()
            .is_none()
            .then_some(approach_request)
            .flatten();
        // A prior natural traversal can leave a landing waiting for a beacon.
        // Once playback is observed before that destination again, that
        // landing belongs to the old traversal and cannot prove a new skip.
        if self
            .awaiting_beacon
            .is_some_and(|awaiting| snapshot.position_ms < awaiting.request.destination.end_ms)
        {
            self.awaiting_beacon = None;
        }
        if self.approach_skip.is_some_and(|request| {
            self.last_settled_skip
                .is_some_and(|settled| settled != request)
        }) {
            self.last_settled_skip = None;
        }
        if self.approach_skip.is_some_and(|request| {
            self.pending_skip
                .is_some_and(|pending| pending.request != request)
        }) {
            self.pending_skip = None;
        }
        if self.approach_skip.is_some_and(|request| {
            self.awaiting_beacon
                .is_some_and(|awaiting| awaiting.request != request)
        }) {
            self.awaiting_beacon = None;
        }
        let inside = approaching.iter().copied().find(|destination| {
            snapshot.position_ms >= destination.start_ms
                && snapshot.position_ms < destination.end_ms
        });
        self.armed_skip = inside.and_then(|destination| self.request_for(destination));
        self.enabled &= !approaching.is_empty();
        if !self.enabled {
            self.deactivate();
        } else {
            for record in &mut self.records {
                if !record.schedulable {
                    record.active = false;
                }
            }
        }
    }

    pub(super) fn candidate(&mut self, manifest: &Manifest) -> Option<MarkerPrewarmCandidate> {
        if !self.enabled {
            return None;
        }
        let mut candidate: Option<MarkerPrewarmCandidate> = None;
        for record in &mut self.records {
            if record.settled || !record.schedulable {
                continue;
            }
            let target_materialized = manifest
                .state(record.destination.target_entry)
                .is_some_and(SegState::is_materialized);
            if target_materialized && !record.credited(record.destination.target_entry) {
                // Ordinary playback or another reader won the race. It is
                // useful media, but it is not this prewarm's production.
                record.active = false;
                record.schedulable = false;
                continue;
            }
            if target_materialized {
                let has_window_gap = manifest
                    .next_gap(record.destination.target_entry)
                    .is_some_and(|gap| gap <= record.destination.window_end_entry);
                if !has_window_gap {
                    record.active = false;
                    record.schedulable = false;
                    continue;
                }
            }
            let proposed = MarkerPrewarmCandidate {
                target_entry: record.destination.target_entry,
                window_end_entry: record.destination.window_end_entry,
                target_materialized,
            };
            if candidate.is_none_or(|current| proposed.target_entry < current.target_entry) {
                candidate = Some(proposed);
            }
        }
        candidate
    }

    pub(super) fn activate(&mut self, candidate: MarkerPrewarmCandidate) -> Vec<u64> {
        for record in &mut self.records {
            record.active = self.enabled
                && record.schedulable
                && !record.settled
                && record.destination.target_entry == candidate.target_entry
                && record.destination.window_end_entry == candidate.window_end_entry;
        }
        self.records
            .iter()
            .filter(|record| record.active)
            .map(|record| record.nonce)
            .collect()
    }

    pub(super) fn deactivate(&mut self) {
        for record in &mut self.records {
            record.active = false;
        }
    }

    pub(super) fn credit_dispatched(
        &mut self,
        candidate: MarkerPrewarmCandidate,
        record_nonce: u64,
        entry: u32,
        publication: u64,
    ) {
        for record in self.records.iter_mut().filter(|record| {
            !record.settled
                && record.nonce == record_nonce
                && record.destination.target_entry == candidate.target_entry
                && record.destination.window_end_entry == candidate.window_end_entry
        }) {
            record.credit(entry, publication);
        }
    }

    pub(super) fn can_match_client_skip(&self) -> bool {
        self.approach_skip.is_some()
            || self.awaiting_beacon.is_some()
            || self.armed_skip.is_some()
            || self.pending_skip.is_some()
            || self.last_settled_skip.is_some()
    }

    fn credited_publication(
        &self,
        request: MarkerPrewarmRequest,
        manifest: &Manifest,
        publications: &[Option<u64>],
    ) -> Option<u64> {
        let publication = publications
            .get(request.destination.target_entry as usize)
            .copied()
            .flatten()?;
        (manifest
            .state(request.destination.target_entry)
            .is_some_and(SegState::is_materialized)
            && self.records.iter().any(|record| {
                record.nonce == request.nonce
                    && record
                        .credited_publication(request.destination.target_entry, Some(publication))
            }))
        .then_some(publication)
    }

    pub(super) fn observe_control_landing(
        &mut self,
        position_ms: i64,
        landing_entry: u32,
        manifest: &Manifest,
        publications: &[Option<u64>],
    ) -> Option<MarkerPrewarmOutcome> {
        if let Some(outcome) = self.settle_pending_at(landing_entry, manifest, publications) {
            return Some(outcome);
        }
        let request = self.armed_skip.or(self.approach_skip).filter(|request| {
            position_ms >= request.destination.end_ms
                && landing_entry == request.destination.target_entry
        })?;
        self.awaiting_beacon = Some(MarkerAwaitingBeacon {
            request,
            publication_at_landing: self.credited_publication(request, manifest, publications),
        });
        None
    }

    pub(super) fn note_client_skip(
        &mut self,
        manifest: &Manifest,
        publications: &[Option<u64>],
    ) -> MarkerClientSkipResult {
        if let Some(awaiting) = self.awaiting_beacon.take() {
            let current = self.credited_publication(awaiting.request, manifest, publications);
            let publication_at_skip = awaiting
                .publication_at_landing
                .filter(|publication| current == Some(*publication));
            self.last_settled_skip = Some(awaiting.request);
            return MarkerClientSkipResult {
                matched: true,
                outcome: Some(self.settle_skip(
                    awaiting.request,
                    publication_at_skip,
                    manifest,
                    publications,
                )),
            };
        }
        if self.pending_skip.is_some() {
            return MarkerClientSkipResult {
                matched: true,
                outcome: None,
            };
        }
        let request = self.armed_skip.take().or(self.approach_skip);
        let Some(request) = request else {
            return MarkerClientSkipResult {
                matched: self.last_settled_skip.is_some(),
                outcome: None,
            };
        };
        if self.approach_skip == Some(request) {
            self.approach_skip = None;
        }
        self.pending_skip = Some(MarkerPendingSkip {
            request,
            publication_at_skip: self.credited_publication(request, manifest, publications),
        });
        MarkerClientSkipResult {
            matched: true,
            outcome: None,
        }
    }

    pub(super) fn settle_pending_at(
        &mut self,
        landing_entry: u32,
        manifest: &Manifest,
        publications: &[Option<u64>],
    ) -> Option<MarkerPrewarmOutcome> {
        let pending = self
            .pending_skip
            .filter(|pending| pending.request.destination.target_entry == landing_entry)?;
        self.pending_skip = None;
        if self.armed_skip == Some(pending.request) {
            self.armed_skip = None;
        }
        self.last_settled_skip = Some(pending.request);
        Some(self.settle_skip(
            pending.request,
            pending.publication_at_skip,
            manifest,
            publications,
        ))
    }

    fn settle_skip(
        &mut self,
        request: MarkerPrewarmRequest,
        publication_at_skip: Option<u64>,
        manifest: &Manifest,
        publications: &[Option<u64>],
    ) -> MarkerPrewarmOutcome {
        self.enabled = false;
        self.deactivate();
        self.approach_skip = None;
        self.armed_skip = None;
        self.awaiting_beacon = None;
        let current_publication = publications
            .get(request.destination.target_entry as usize)
            .copied()
            .flatten();
        let current_matches = publication_at_skip.is_some()
            && current_publication == publication_at_skip
            && manifest
                .state(request.destination.target_entry)
                .is_some_and(SegState::is_materialized);
        let record = self
            .records
            .iter_mut()
            .find(|record| record.nonce == request.nonce);
        let (requested_sequence, produced_range) = record.map_or((None, None), |record| {
            record.settled = true;
            record.schedulable = false;
            (
                Some(record.requested_sequence),
                (current_matches
                    && record.credited_publication(
                        request.destination.target_entry,
                        publication_at_skip,
                    ))
                .then_some(PrewarmedRange {
                    first: request.destination.target_entry,
                    last: request.destination.target_entry,
                }),
            )
        });
        MarkerPrewarmOutcome {
            hit: produced_range.is_some(),
            destination: request.destination,
            requested_sequence,
            produced_range,
        }
    }
}
