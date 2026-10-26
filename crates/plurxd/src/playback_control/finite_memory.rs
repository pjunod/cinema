//! Requested heap capacity of control values retained by a finite VOD
//! attachment. These measurements describe owned Rust payloads, not process
//! RSS or the allocator's private bookkeeping.

#![cfg_attr(
    all(test, not(target_os = "linux")),
    allow(
        dead_code,
        reason = "portable private-token model fixtures exercise the shared pure compute/measure/adopt step; remaining finite production projections are composed only on Linux"
    )
)]

use super::*;

/// Pinned Rust1.97.1 ArcInner is repr(C): two pointer-sized atomic
/// refcounts followed by T. Extend and pad like alloc::sync does; merely
/// adding header+payload misses trailing alignment for Arc<AtomicBool>.
/// Source: rust-lang/rust 8bab26f4f68e0e26f0bb7960be334d5b520ea452,
/// library/alloc/src/sync.rs:365-386.
pub(crate) fn arc_requested_layout(payload: std::alloc::Layout) -> Option<usize> {
    std::alloc::Layout::new::<[std::sync::atomic::AtomicUsize; 2]>()
        .extend(payload)
        .ok()
        .map(|(layout, _)| layout.pad_to_align().size())
}

pub(super) fn arc_requested_bytes<T>() -> Option<usize> {
    arc_requested_layout(std::alloc::Layout::new::<T>())
}

fn add(left: usize, right: usize) -> Option<usize> {
    left.checked_add(right)
}

fn vec_bytes<T>(value: &Vec<T>) -> Option<usize> {
    value.capacity().checked_mul(std::mem::size_of::<T>())
}

fn string(value: &Option<String>) -> usize {
    value.as_ref().map_or(0, String::capacity)
}

impl PlaybackDemandSnapshot {
    pub(crate) fn requested_heap_bytes(&self) -> Option<usize> {
        let mut bytes = self
            .intent
            .as_ref()
            .map_or(0, |intent| intent.lifetime_id.capacity());
        bytes = add(bytes, string(&self.request_fingerprint))?;
        if let Some(capabilities) = &self.capabilities {
            bytes = add(bytes, capabilities.requested_heap_bytes()?)?;
        }
        if let Some(observation) = &self.observation {
            bytes = add(bytes, string(&observation.error_detail))?;
        }
        if let Some(acknowledgement) = &self.acknowledgement {
            bytes = add(bytes, acknowledgement.action_id.capacity())?;
        }
        Some(bytes)
    }
}

impl DynamicCapabilities {
    pub(crate) fn requested_heap_bytes(&self) -> Option<usize> {
        let mut bytes = add(vec_bytes(&self.codecs)?, vec_bytes(&self.dynamic_ranges)?)?;
        if let Some(snapshot) = &self.decoder_caps {
            bytes = add(bytes, vec_bytes(&snapshot.video)?)?;
            for entry in &snapshot.video {
                bytes = add(bytes, entry.codec.capacity())?;
                bytes = add(bytes, vec_bytes(&entry.profiles)?)?;
                for profile in &entry.profiles {
                    bytes = add(bytes, profile.capacity())?;
                }
                bytes = add(bytes, vec_bytes(&entry.dynamic_ranges)?)?;
                bytes = add(bytes, vec_bytes(&entry.dv_profiles)?)?;
            }
        }
        Some(bytes)
    }
}

impl DecoderCapsSnapshot {
    pub(crate) fn requested_heap_bytes(&self) -> Option<usize> {
        let mut bytes = vec_bytes(&self.video)?;
        for entry in &self.video {
            bytes = add(bytes, entry.codec.capacity())?;
            bytes = add(bytes, vec_bytes(&entry.profiles)?)?;
            for profile in &entry.profiles {
                bytes = add(bytes, profile.capacity())?;
            }
            bytes = add(bytes, vec_bytes(&entry.dynamic_ranges)?)?;
            bytes = add(bytes, vec_bytes(&entry.dv_profiles)?)?;
        }
        Some(bytes)
    }
}

impl ControlBootstrap {
    pub(crate) fn requested_heap_bytes(&self) -> Option<usize> {
        add(
            add(self.protocol.capacity(), self.url.capacity())?,
            self.generation.capacity(),
        )
    }
}

impl EffectiveSelection {
    fn requested_heap_bytes(&self) -> Option<usize> {
        add(self.codec.capacity(), string(&self.dynamic_range))
    }
}

fn enhancement_bytes(
    value: &plurx_core::playback::enhancement::PlaybackEnhancement,
) -> Option<usize> {
    let mut bytes = add(vec_bytes(&value.active)?, vec_bytes(&value.details)?)?;
    for detail in &value.details {
        bytes = add(bytes, detail.text.capacity())?;
    }
    Some(bytes)
}

impl PreparedSuccessorAction {
    fn requested_heap_bytes(&self) -> Option<usize> {
        let mut bytes = add(
            self.staged_incarnation_id.capacity(),
            self.session_id.capacity(),
        )?;
        bytes = add(bytes, self.playlist_url.capacity())?;
        bytes = add(bytes, self.effective_selection.requested_heap_bytes()?)?;
        if let Some(control) = &self.control {
            bytes = add(bytes, std::mem::size_of::<ControlBootstrap>())?;
            bytes = add(bytes, control.requested_heap_bytes()?)?;
        }
        if let Some(enhancement) = &self.enhancement {
            bytes = add(
                bytes,
                std::mem::size_of::<plurx_core::playback::enhancement::PlaybackEnhancement>(),
            )?;
            bytes = add(bytes, enhancement_bytes(enhancement)?)?;
        }
        Some(bytes)
    }
}

impl ControlAction {
    pub(crate) fn requested_heap_bytes(&self) -> Option<usize> {
        match self {
            Self::None | Self::Hold { .. } | Self::RetryResource { .. } => Some(0),
            Self::Terminal { message, .. } => Some(message.capacity()),
            Self::Prepare {
                action_id,
                session_id,
                playlist_url,
                control,
                effective_selection,
                enhancement,
                ..
            } => {
                let mut bytes = add(action_id.capacity(), session_id.capacity())?;
                bytes = add(bytes, playlist_url.capacity())?;
                bytes = add(bytes, effective_selection.requested_heap_bytes()?)?;
                if let Some(control) = control {
                    bytes = add(bytes, std::mem::size_of::<ControlBootstrap>())?;
                    bytes = add(bytes, control.requested_heap_bytes()?)?;
                }
                if let Some(enhancement) = enhancement {
                    bytes = add(
                        bytes,
                        std::mem::size_of::<plurx_core::playback::enhancement::PlaybackEnhancement>(
                        ),
                    )?;
                    bytes = add(bytes, enhancement_bytes(enhancement)?)?;
                }
                Some(bytes)
            }
        }
    }
}

impl PreparedActionBinding {
    fn requested_heap_bytes(&self) -> Option<usize> {
        let mut bytes = self.successor.requested_heap_bytes()?;
        bytes = add(bytes, self.action.requested_heap_bytes()?)?;
        if let Some(acknowledgement) = &self.acknowledgement {
            bytes = add(bytes, acknowledgement.action_id.capacity())?;
        }
        Some(bytes)
    }
}

impl PreparationDirective {
    pub(super) fn requested_finite_heap_bytes(&self) -> usize {
        self.requested_heap_bytes()
    }
    fn requested_heap_bytes(&self) -> usize {
        match self {
            Self::Commit {
                staged_incarnation_id,
            }
            | Self::Abort {
                staged_incarnation_id,
                ..
            } => staged_incarnation_id.capacity(),
        }
    }
}

impl PreparationSlot {
    fn requested_heap_bytes(&self) -> Option<usize> {
        match self {
            Self::Empty => Some(0),
            Self::Staged {
                staged_incarnation_id,
                predecessor_incarnation_id,
                desired_digest,
                ..
            } => add(
                add(
                    staged_incarnation_id.capacity(),
                    predecessor_incarnation_id.capacity(),
                )?,
                string(desired_digest),
            ),
            Self::Committing {
                staged_incarnation_id,
                ..
            }
            | Self::Aborting {
                staged_incarnation_id,
            } => Some(staged_incarnation_id.capacity()),
        }
    }
}

impl SelectionObservation {
    pub(crate) fn requested_heap_bytes(&self) -> Option<usize> {
        let mut bytes = 0usize;
        if let Some(persist) = &self.persist_desired {
            bytes = add(persist.digest.capacity(), persist.canonical_form.capacity())?;
        }
        if let Some(capabilities) = &self.capabilities {
            bytes = add(bytes, capabilities.requested_heap_bytes()?)?;
        }
        Some(bytes)
    }
}

impl ControlState {
    pub(crate) fn requested_heap_bytes(&self) -> Option<usize> {
        let mut bytes = arc_requested_bytes::<AtomicBool>()?.checked_mul(2)?;
        for value in [
            &self.generation,
            &self.prior_request_fingerprint,
            &self.persisted_digest,
            &self.dispatched_digest,
            &self.desired_digest,
        ] {
            bytes = add(bytes, string(value))?;
        }
        bytes = add(bytes, self.prior_action.requested_heap_bytes()?)?;
        if let Some(directive) = &self.prior_preparation_directive {
            bytes = add(bytes, directive.requested_heap_bytes())?;
        }
        if let Some(binding) = &self.prepared_action {
            bytes = add(bytes, binding.requested_heap_bytes()?)?;
        }
        if let Some(capabilities) = &self.last_capabilities {
            bytes = add(bytes, capabilities.requested_heap_bytes()?)?;
        }
        if let Some(capabilities) = &self.preparation_capabilities {
            bytes = add(bytes, capabilities.requested_heap_bytes()?)?;
        }
        add(bytes, self.preparation.requested_heap_bytes()?)
    }
}

impl ControlState {
    /// Payload capacity needed while a detached control step is being built,
    /// before its final retained state can be measured. The terms below name
    /// the clone sites in ControlAcceptance::observed, accept_step_detached,
    /// accept_in_place, ControlState::observe, and preparation_directive.
    /// The caller holds a separate short-lived client slot through adoption.
    pub(crate) fn finite_prepare_transient_bytes(
        &self,
        generation: &str,
        snapshot: &PlaybackDemandSnapshot,
        prepared_successor: &PreparedSuccessorObservation,
    ) -> Option<usize> {
        let mut bytes = self.requested_heap_bytes()?; // detached next = self.clone()
                                                      // Two fresh observational Arc<AtomicBool> tokens in accept_step_detached.
        bytes = add(bytes, arc_requested_bytes::<AtomicBool>()?.checked_mul(2)?)?;
        bytes = add(bytes, generation.len())?; // first accepted generation
        bytes = add(bytes, string(&snapshot.request_fingerprint))?; // acceptance input

        let incoming = snapshot
            .capabilities
            .as_ref()
            .map_or(Some(0), DynamicCapabilities::requested_heap_bytes)?;
        let previous = self
            .last_capabilities
            .as_ref()
            .map_or(Some(0), DynamicCapabilities::requested_heap_bytes)?;
        // Acceptance copies incoming once. The accept and observe merges each
        // copy a document and may fill an absent decoder from the prior one;
        // observation returns one final copy. Adding four of each source
        // covers these named simultaneous copies even on a stale refusal.
        for _ in 0..4 {
            bytes = add(bytes, incoming)?;
            bytes = add(bytes, previous)?;
        }
        let prior_action = self.prior_action.requested_heap_bytes()?;
        bytes = add(bytes, prior_action)?; // replay or accepted result clone
        if let Some(binding) = &self.prepared_action {
            bytes = add(bytes, binding.action.requested_heap_bytes()?)?;
        }
        if let PreparedSuccessorObservation::Ready(successor) = prepared_successor {
            let payload = successor.requested_heap_bytes()?;
            // Acceptance owns one Box<Successor>, then the new binding owns a
            // successor copy. The conversion creates an action, followed by
            // binding and result action clones. Each action's string payload
            // is bounded by the successor payload plus its UUID action id.
            bytes = add(bytes, std::mem::size_of::<PreparedSuccessorAction>())?;
            for _ in 0..5 {
                bytes = add(bytes, payload)?;
            }
            bytes = add(bytes, 3 * 36)?; // action-id UUIDs
        }
        if let Some(ack) = &snapshot.acknowledgement {
            // Owned acceptance, binding progress/terminal update, and returned
            // directive's bound acknowledgement path.
            for _ in 0..3 {
                bytes = add(bytes, ack.action_id.capacity())?;
            }
        }
        let staged = self.preparation.staged_incarnation_id().map_or(0, str::len);
        // Owner rollover and terminal acknowledgement may duplicate the
        // staged id into an abort slot, directive and returned directive.
        for _ in 0..5 {
            bytes = add(bytes, staged)?;
        }
        // DesiredSelection::canonical_form uses fixed labels, at most one
        // 32-hex candidate id and signed 64-bit decimal fields. The format
        // below is <256 bytes even at all field maxima. Acceptance and
        // observation each compute a SHA-256 hex digest; observation can also
        // retain the canonical form. Count their formatting scratch and
        // returned 64-byte digests before either computation starts.
        for _ in 0..3 {
            bytes = add(bytes, 256)?;
        }
        for _ in 0..3 {
            bytes = add(bytes, 64)?;
        }
        Some(bytes)
    }
}

/// A control exchange computed against private liveness tokens. A finite
/// caller may reserve its entire next retained payload before accepting the
/// sequence. Dropping this value leaves the published fence untouched.
pub(crate) struct PreparedFiniteControl {
    next: ControlState,
    result: Result<Disposition, ControlStateError>,
    private: DetachedObservationTokens,
    observation: SelectionObservation,
    directive: Option<PreparationDirective>,
}

type AdoptedFiniteControl = (
    Result<DispositionTuple, ControlStateError>,
    SelectionObservation,
    Option<PreparationDirective>,
);

impl PreparedFiniteControl {
    pub(crate) fn returns_result(&self) -> bool {
        self.result.is_ok()
    }

    pub(crate) fn accepted(&self) -> bool {
        matches!(
            &self.result,
            Ok(Disposition {
                disposition: ControlDisposition::Accepted,
                ..
            })
        )
    }

    pub(crate) fn external_payload_bytes(&self) -> Option<usize> {
        let mut bytes = self
            .result
            .as_ref()
            .ok()
            .map_or(Some(0), |result| result.action.requested_heap_bytes())?;
        bytes = add(bytes, self.observation.requested_heap_bytes()?)?;
        if let Some(directive) = &self.directive {
            bytes = add(bytes, directive.requested_heap_bytes())?;
        }
        Some(bytes)
    }

    pub(crate) fn requested_heap_bytes(&self) -> Option<usize> {
        add(
            self.next.requested_heap_bytes()?,
            self.external_payload_bytes()?,
        )
    }

    pub(crate) fn adopt(self, published: &mut ControlState) -> AdoptedFiniteControl {
        let Self {
            mut next,
            result,
            private,
            observation,
            directive,
        } = self;
        adopt_observation_token(
            &published.observational_stage_live,
            &mut next.observational_stage_live,
            &private.stage_live,
        );
        adopt_observation_token(
            &published.observational_live,
            &mut next.observational_live,
            &private.live,
        );
        *published = next;
        (result.map(Disposition::into_tuple), observation, directive)
    }
}

impl ControlState {
    pub(crate) fn prepare_finite_control(
        &self,
        request: ControlRequestView<'_>,
        snapshot: &PlaybackDemandSnapshot,
    ) -> PreparedFiniteControl {
        let (mut next, result, private) = accept_step_detached(self, Instant::now(), request);
        let observation = if matches!(
            &result,
            Ok(Disposition {
                disposition: ControlDisposition::Accepted,
                ..
            })
        ) {
            next.observe(&snapshot.selection, snapshot.capabilities.as_ref())
        } else {
            SelectionObservation::default()
        };
        let directive = next.preparation_directive();
        PreparedFiniteControl {
            next,
            result,
            private,
            observation,
            directive,
        }
    }
}

impl crate::vodserve::VodSessionInfo {
    pub(crate) fn requested_heap_bytes(&self) -> Option<usize> {
        let mut bytes = add(self.id.capacity(), string(&self.producer_failed))?;
        if let Some(format) = &self.stream_format {
            for value in [&format.rendition_id, &format.video_codec] {
                bytes = add(bytes, value.capacity())?;
            }
            for value in [&format.encoder, &format.dynamic_range, &format.ffmpeg_build] {
                bytes = add(bytes, string(value))?;
            }
        }
        Some(bytes)
    }
}

impl LocalControlResult {
    pub(crate) fn requested_vod_heap_bytes(&self) -> Option<usize> {
        let mut bytes = self.action.requested_heap_bytes()?;
        if let Some(directive) = &self.preparation_directive {
            bytes = add(bytes, directive.requested_heap_bytes())?;
        }
        bytes = add(bytes, self.selection.requested_heap_bytes()?)?;
        if let HlsSessionInfo::Vod(status) = &self.status {
            bytes = add(
                bytes,
                std::mem::size_of::<crate::vodserve::VodSessionInfo>(),
            )?;
            bytes = add(bytes, status.requested_heap_bytes()?)?;
            Some(bytes)
        } else {
            None
        }
    }
}

impl ControlState {
    /// Upper increment for stage's owned arguments and capability clone.
    /// Replacing the observational token is a fixed-size exchange already
    /// counted by `requested_heap_bytes`.
    pub(crate) fn finite_stage_growth_bytes(
        &self,
        staged: &str,
        predecessor: &str,
        desired: Option<&str>,
    ) -> Option<usize> {
        let mut bytes = add(staged.len(), predecessor.len())?;
        bytes = add(bytes, desired.map_or(0, str::len))?;
        if let Some(capabilities) = &self.last_capabilities {
            bytes = add(bytes, capabilities.requested_heap_bytes()?)?;
        }
        Some(bytes)
    }
}

impl LocalControlResult {
    /// Requested heap behind one independent result clone. Shared terminal
    /// receipts and handoff Arcs keep their own original allocation; each
    /// clone pays for the owned action, directive, selection, and status.
    pub(crate) fn requested_clone_heap_bytes(&self) -> Option<usize> {
        let mut bytes = std::mem::size_of::<Self>();
        bytes = add(bytes, self.action.requested_heap_bytes()?)?;
        if let Some(directive) = &self.preparation_directive {
            bytes = add(bytes, directive.requested_heap_bytes())?;
        }
        bytes = add(bytes, self.selection.requested_heap_bytes()?)?;
        match &self.status {
            crate::transcode::HlsSessionInfo::Vod(status) => {
                bytes = add(
                    bytes,
                    std::mem::size_of::<crate::vodserve::VodSessionInfo>(),
                )?;
                bytes = add(bytes, status.requested_heap_bytes()?)?;
            }
            crate::transcode::HlsSessionInfo::Live(status) => {
                bytes = add(bytes, std::mem::size_of::<crate::transcode::SessionInfo>())?;
                bytes = add(bytes, status.requested_heap_bytes()?)?;
            }
        }
        Some(bytes)
    }
}

impl ControlResponseV1 {
    pub(crate) fn requested_heap_bytes(&self) -> Option<usize> {
        let mut bytes = std::mem::size_of::<Self>();
        bytes = add(bytes, self.protocol.capacity())?;
        bytes = add(bytes, self.generation.capacity())?;
        bytes = add(bytes, self.lease.state.capacity())?;
        for value in [
            &self.delivery.presentation,
            &self.delivery.producer_state,
            &self.delivery.owner_node_hash,
        ] {
            bytes = add(bytes, value.capacity())?;
        }
        for value in [
            &self.delivery.hold_reason,
            &self.delivery.producer_decision,
            &self.delivery.subtitle_readiness,
            &self.delivery.subtitle_revision,
            &self.delivery.preparation,
        ] {
            bytes = add(bytes, string(value))?;
        }
        bytes = add(bytes, self.effective_selection.requested_heap_bytes()?)?;
        add(bytes, self.action.requested_heap_bytes()?)
    }
}

impl ControlState {
    pub(super) fn initial_finite_heap_bytes() -> Option<usize> {
        // Exactly the two Arc<AtomicBool> allocations in Default. No state
        // constructor is invoked before this reservation is taken.
        arc_requested_bytes::<AtomicBool>()?.checked_mul(2)
    }
}

impl RollingProducerOperationalSnapshot {
    pub(crate) fn requested_heap_bytes(&self) -> Option<usize> {
        let RollingProducerOperationalSnapshot {
            phase: _,
            deadline_attempt: _,
            deadline_mode: _,
            deadline_remaining_ms: _,
            due_attempt: _,
            due_mode: _,
            due_overdue_ms: _,
            process_exit_attempt: _,
            process_exit_due: _,
            physical_flow: _,
            last_flow_applied_sequence: _,
            last_applied_sequence: _,
            observation_only: _,
            action_owner: _,
            startup_kind: _,
            presentation_contract_fingerprint,
            metadata_response_authorized: _,
            producer_media_published: _,
            retry_state: _,
            decision_sequence: _,
            decision_reason: _,
            executor_state: _,
            executor_pending_decision_age_ms: _,
            executor_last_observed_sequence: _,
            executor_last_action_failure: _,
            executor_registered: _,
            decision_applied_sequence: _,
            decision_installed_attempt: _,
            pending_probe_sequence: _,
            pending_probe_attempt: _,
            pending_probe_deadline_remaining_ms: _,
            last_probe_outcome: _,
            producer_ended_with_proposal: _,
            proposal,
            decode_fault: _,
            decode_error_records: _,
            diagnostics_qualification: _,
            completion: _,
            completion_attempt: _,
            completion_final_segment: _,
            completion_final_end_ms: _,
        } = self;
        add(
            string(presentation_contract_fingerprint),
            proposal
                .as_ref()
                .map_or(0, |value| value.proposal_id.capacity()),
        )
    }
}

impl RollingLeaseSnapshot {
    pub(crate) fn requested_heap_bytes(&self) -> Option<usize> {
        let RollingLeaseSnapshot {
            mode: _,
            idle_for: _,
            remaining: _,
            deadline: _,
            last_renewal_kind: _,
            demand,
            accepted_demand_sequence: _,
            demand_observation_age: _,
            settled_target: _,
            startup: _,
            pause_remaining: _,
            delivery: _,
            retired: _,
            terminal: _,
            expiration_claimed: _,
            producer_control,
        } = self;
        add(
            demand
                .as_ref()
                .map_or(Some(0), PlaybackDemandSnapshot::requested_heap_bytes)?,
            producer_control.requested_heap_bytes()?,
        )
    }
}

impl RollingControlOutcome {
    pub(crate) fn requested_clone_heap_bytes(&self) -> Option<usize> {
        let RollingControlOutcome {
            disposition: _,
            accepted_sequence: _,
            action,
            action_suppressed: _,
            preparation_directive,
            platform: _,
            lease,
            flow_ticket: _,
            selection,
            allocation: _,
        } = self;
        let mut bytes = add(std::mem::size_of::<Self>(), action.requested_heap_bytes()?)?;
        bytes = add(
            bytes,
            preparation_directive
                .as_ref()
                .map_or(0, PreparationDirective::requested_heap_bytes),
        )?;
        bytes = add(bytes, lease.requested_heap_bytes()?)?;
        add(bytes, selection.requested_heap_bytes()?)
    }
}

impl crate::transcode::SessionInfo {
    pub(crate) fn requested_heap_bytes(&self) -> Option<usize> {
        let crate::transcode::SessionInfo {
            vod_observation,
            id,
            presentation: _,
            file_id: _,
            item_id: _,
            item_title,
            user_name,
            target_height: _,
            encoder: _,
            tone_map_peak_nits: _,
            tone_map_peak_source: _,
            started_unix: _,
            idle_seconds: _,
            last_request: _,
            lease_mode: _,
            lease_state: _,
            lease_timeout_ms: _,
            startup_state: _,
            startup_remaining_ms: _,
            presentation_progress_seen: _,
            produced_end_ms: _,
            served_end_ms: _,
            budget_anchor_sequence: _,
            allowed_end_ms: _,
            carried_surplus_ms: _,
            demand_observation_age_ms: _,
            staged_bytes: _,
            playlist_target_ms: _,
            served_revision: _,
            last_segment_advanced_idle_ms: _,
            next_publication_in_ms: _,
            publication_deadline_remaining_ms: _,
            maintenance_state: _,
            rate_estimate_source: _,
            estimate_active_speed: _,
            pause_grace_remaining_ms: _,
            retirement_reason: _,
            advertised_bytes: _,
            grace_bytes: _,
            reserved_bytes: _,
            live_bytes: _,
            control_demand: _,
            reported_position_ms: _,
            client_runway_ms: _,
            render_state: _,
            server_ready_state: _,
            server_ready_anchor_ms: _,
            server_ready_end_ms: _,
            server_ready_seconds: _,
            server_next_ready_start_ms: _,
            server_next_ready_end_ms: _,
            production_policy: _,
            production_ahead_seconds: _,
            production_target_seconds: _,
            producer_control,
            producer_state: _,
            producer_attempt: _,
            playlist_ready: _,
            published_segment: _,
            next_media_sequence: _,
            pending_fetched_segment: _,
            speed: _,
            recent_speed: _,
            out_time_ms: _,
            progress_idle_ms: _,
            producer_exit_success: _,
            producer_exit_code: _,
            producer_exit_signal: _,
            producer_exit_idle_ms: _,
            published_end_ms: _,
            fetched_end_ms: _,
            fetched_segment: _,
            first_retained_segment: _,
            playlist_shape: _,
            ahead_seconds: _,
            hold_reason: _,
            resume_below_seconds: _,
            resume_below_bytes: _,
            ahead_bytes: _,
            delivered_bytes: _,
            delivered_bps: _,
            delivered_idle_ms: _,
            http_wait_count: _,
            http_wait_oldest_ms: _,
            http_wait_segment: _,
            status_generated_unix_ms: _,
            readrate: _,
            suspended: _,
            suspend_count: _,
        } = self;
        // Selected rolling status has no VOD observation sidecar. Any new
        // shape refuses until its complete owned field inventory is added.
        if vod_observation.is_some() {
            return None;
        }
        let mut bytes = add(id.capacity(), item_title.capacity())?;
        bytes = add(bytes, user_name.capacity())?;
        add(
            bytes,
            producer_control.as_ref().map_or(
                Some(0),
                RollingProducerOperationalSnapshot::requested_heap_bytes,
            )?,
        )
    }
}

impl PreparedFiniteControl {
    pub(super) fn retained_state_bytes(&self) -> Option<usize> {
        self.next.requested_heap_bytes()
    }
}

impl ControlState {
    /// Same private-token step as finite VOD, preserving the actor's original
    /// directive-before-observation order and published ingress timestamp.
    pub(super) fn prepare_finite_actor_control_at(
        &self,
        now: Instant,
        request: ControlRequestView<'_>,
        snapshot: &PlaybackDemandSnapshot,
    ) -> PreparedFiniteControl {
        let (mut next, result, private) = accept_step_detached(self, now, request);
        let directive = next.preparation_directive();
        let observation = if matches!(
            &result,
            Ok(Disposition {
                disposition: ControlDisposition::Accepted,
                ..
            })
        ) {
            next.observe(&snapshot.selection, snapshot.capabilities.as_ref())
        } else {
            SelectionObservation::default()
        };
        PreparedFiniteControl {
            next,
            result,
            private,
            observation,
            directive,
        }
    }
}

#[cfg(target_os = "linux")]
impl RollingControlActor {
    /// The only owned strings built by producer_operational_snapshot_at are
    /// clones of these actual retained fingerprint/proposal sources.
    pub(super) fn finite_operational_snapshot_bound(&self) -> Option<usize> {
        let mut bytes = self
            .prepublication
            .as_ref()
            .and_then(|value| value.initial_policy.as_ref())
            .map_or(0, |value| {
                value.presentation_contract_fingerprint.capacity()
            });
        bytes = add(
            bytes,
            self.response_publication_contract
                .as_ref()
                .map_or(0, |value| {
                    value.presentation_contract_fingerprint.capacity()
                }),
        )?;
        let proposal = self
            .pending_decision
            .as_ref()
            .or(self.last_decision.as_ref())
            .and_then(|value| match value.as_ref() {
                ProducerDecision::Fail { proposal, .. } => proposal.as_ref(),
                _ => None,
            });
        add(
            bytes,
            proposal.map_or(0, |value| value.proposal_id.capacity()),
        )
    }

    pub(super) fn finite_snapshot_control_bound(
        &self,
        request: &OwnedLocalControlRequest,
    ) -> Option<usize> {
        // claim_expiry_at can clone the old demand; an accepted exchange's
        // final snapshot clones the incoming one. Both are real sources.
        let old = self
            .demand
            .as_ref()
            .map_or(Some(0), PlaybackDemandSnapshot::requested_heap_bytes)?;
        add(
            add(old, request.snapshot.requested_heap_bytes()?)?,
            self.finite_operational_snapshot_bound()?,
        )
    }

    pub(super) fn finite_control_transient_bound(
        &self,
        request: &OwnedLocalControlRequest,
    ) -> Option<usize> {
        let mut bytes = self.control.finite_prepare_transient_bytes(
            &request.generation,
            &request.snapshot,
            &request.prepared_successor,
        )?;
        // claim_expiry_at always builds one old snapshot and may build a
        // second committed terminal snapshot before dropping the first.
        let snapshot = self.finite_snapshot_control_bound(request)?;
        bytes = add(bytes, snapshot.checked_mul(2)?)?;
        bytes = add(bytes, std::mem::size_of::<PreparedFiniteControl>())?;
        bytes = add(bytes, std::mem::size_of::<RollingControlOutcome>())?;
        // Actor retention duplicates accepted capabilities independently of
        // ControlState::observe; clone_from can retain the previous capacity.
        bytes = add(
            bytes,
            self.retained_capabilities
                .as_ref()
                .map_or(Some(0), DynamicCapabilities::requested_heap_bytes)?,
        )?;
        bytes = add(
            bytes,
            request
                .snapshot
                .capabilities
                .as_ref()
                .map_or(Some(0), DynamicCapabilities::requested_heap_bytes)?,
        )?;
        add(bytes, arc_requested_bytes::<AtomicBool>()?) // terminal observation-token replacement
    }
}

impl OwnedLocalControlRequest {
    pub(super) fn projected_borrowed_bytes(request: &LocalControlRequest<'_>) -> Option<usize> {
        let LocalControlRequest {
            generation,
            client_instance_id,
            snapshot,
            prepared_successor,
            session_id: _,
            owner_node_id: _,
            owner_epoch: _,
            sequence: _,
        } = request;
        let mut bytes = add(std::mem::size_of::<Self>(), generation.len())?;
        bytes = add(bytes, client_instance_id.len())?;
        bytes = add(bytes, snapshot.requested_heap_bytes()?)?;
        if let PreparedSuccessorObservation::Ready(value) = prepared_successor {
            bytes = add(bytes, std::mem::size_of::<PreparedSuccessorAction>())?;
            bytes = add(bytes, value.requested_heap_bytes()?)?;
        }
        Some(bytes)
    }
}

#[cfg(target_os = "linux")]
pub(super) struct PreparedActorAccounting {
    pub(super) _transient: ActorPayloadAllocation,
    pub(super) state: ActorPayloadAllocation,
    pub(super) reply: ActorPayloadAllocation,
    pub(super) terminal: Option<ActorPayloadAllocation>,
    pub(super) preparation: Option<ActorPayloadAllocation>,
}

#[cfg(target_os = "linux")]
impl RollingControlActor {
    pub(super) fn prepare_finite_actor_accounting(
        &mut self,
        now: Instant,
        request: &OwnedLocalControlRequest,
        terminal: bool,
        preparation: bool,
    ) -> Result<Option<PreparedActorAccounting>, ()> {
        let Some(budget) = self.reply_budget.as_ref().map(Arc::clone) else {
            return Ok(None);
        };
        let transient = budget.reserve(self.finite_control_transient_bound(request).ok_or(())?)?;
        let plan = self.control.prepare_finite_actor_control_at(
            now,
            ControlRequestView {
                generation: &request.generation,
                owner_epoch: request.owner_epoch,
                client_instance_id: &request.client_instance_id,
                sequence: request.sequence,
                acceptance: ControlAcceptance::observed(
                    request.snapshot.platform(),
                    &request.prepared_successor,
                    request.snapshot.acknowledgement.as_ref(),
                    request.snapshot.request_fingerprint.as_deref(),
                    &request.snapshot.selection,
                    request.snapshot.capabilities.as_ref(),
                ),
            },
            &request.snapshot,
        );
        transient.validate_actual(plan.requested_heap_bytes().ok_or(())?)?;
        let incoming = request.snapshot.requested_heap_bytes().ok_or(())?;
        let retained_caps = plan
            .next
            .last_capabilities
            .as_ref()
            .map_or(Some(0), DynamicCapabilities::requested_heap_bytes)
            .ok_or(())?;
        let state_bytes = add(
            add(plan.retained_state_bytes().ok_or(())?, incoming).ok_or(())?,
            retained_caps,
        )
        .ok_or(())?;
        let state = budget.reserve(state_bytes)?;
        // Current state covers an exact terminal replay before the private
        // plan is adopted. External plan payload covers an accepted exchange.
        let external = add(
            plan.external_payload_bytes().ok_or(())?,
            self.control.requested_heap_bytes().ok_or(())?,
        )
        .ok_or(())?;
        let reply_bytes = add(
            add(std::mem::size_of::<RollingControlOutcome>(), external).ok_or(())?,
            self.finite_snapshot_control_bound(request).ok_or(())?,
        )
        .ok_or(())?;
        let reply = budget.reserve(reply_bytes)?;
        let terminal = if terminal {
            Some(budget.reserve(reply_bytes)?)
        } else {
            None
        };
        let preparation = if preparation {
            let bytes = add(std::mem::size_of::<PreparationControlOutcome>(), external).ok_or(())?;
            Some(budget.reserve(bytes)?)
        } else {
            None
        };
        // All reservations precede adoption, including all mandatory clones.
        self.prepared_control = Some(plan);
        Ok(Some(PreparedActorAccounting {
            _transient: transient,
            state,
            reply,
            terminal,
            preparation,
        }))
    }
}

#[cfg(test)]
mod allocation_layout_tests {
    use super::*;
    #[test]
    fn atomic_bool_arc_projection_includes_refcount_alignment() {
        let word = std::mem::size_of::<usize>();
        assert_eq!(arc_requested_bytes::<AtomicBool>(), Some(3 * word));
        assert_eq!(ControlState::initial_finite_heap_bytes(), Some(6 * word));
    }
    #[test]
    fn arc_projection_preserves_payload_alignment_and_final_padding() {
        #[repr(C, align(64))]
        struct Aligned {
            byte: u8,
        }
        let value = Aligned { byte: 1 };
        assert_eq!(value.byte, 1);
        assert_eq!(arc_requested_bytes::<Aligned>(), Some(128));
    }

    #[test]
    fn rolling_prepare_refusal_keeps_original_state_and_tokens_live() {
        // A pure private-plan test: no physical receipt or native execution.
        let snapshot = PlaybackDemandSnapshot {
            intent: None,
            request_fingerprint: Some("same original request".to_owned()),
            demand: PlaybackDemand::Active,
            position_ms: 0,
            buffered_from_ms: None,
            buffered_through_ms: 0,
            playback_rate: 1.0,
            render_state: RenderState::Starting,
            seek_target_ms: None,
            observed_download_bps: None,
            selection: ClientSelection {
                quality: QualitySelection::Auto {
                    height: None,
                    candidate_id: None,
                },
                audio_track: None,
                subtitle: SubtitleSelection {
                    mode: SubtitleMode::Off,
                    track: None,
                },
                audio_offset_ms: 0,
                codec: CodecPolicy::Auto,
                dynamic_range: DynamicRangePolicy::Auto,
            },
            capabilities: None,
            observation: None,
            acknowledgement: None,
        };
        let request = || ControlRequestView {
            generation: "original rolling generation",
            owner_epoch: 1,
            client_instance_id: "22222222-2222-4222-8222-222222222222",
            sequence: 1,
            acceptance: ControlAcceptance::observed(
                None,
                &PreparedSuccessorObservation::NotRequested,
                None,
                snapshot.request_fingerprint.as_deref(),
                &snapshot.selection,
                None,
            ),
        };
        let mut state = ControlState::default();
        let original_live = Arc::clone(&state.observational_live);
        let original_stage = Arc::clone(&state.observational_stage_live);
        let now = Instant::now();
        let refused = state.prepare_finite_actor_control_at(now, request(), &snapshot);
        assert!(refused.accepted());
        assert!(refused.requested_heap_bytes().is_some());
        drop(refused); // The actual caller refuses a slot before adoption.
        assert_eq!(state.last_sequence, 0);
        assert!(state.generation.is_none());
        assert!(Arc::ptr_eq(&original_live, &state.observational_live));
        assert!(Arc::ptr_eq(
            &original_stage,
            &state.observational_stage_live
        ));
        assert!(original_live.load(Ordering::Acquire));
        let admitted = state.prepare_finite_actor_control_at(now, request(), &snapshot);
        let (result, _, _) = admitted.adopt(&mut state);
        assert_eq!(
            result.expect("private plan accepted").0,
            ControlDisposition::Accepted
        );
        assert_eq!(state.last_sequence, 1);
    }
}
