//! Requested heap capacity of control values retained by a finite VOD
//! attachment. These measurements describe owned Rust payloads, not process
//! RSS or the allocator's private bookkeeping.

use super::*;

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
        let mut bytes = 2usize
            .checked_mul(std::mem::size_of::<AtomicBool>() + 2 * std::mem::size_of::<usize>())?;
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
        bytes = add(
            bytes,
            2usize.checked_mul(
                std::mem::size_of::<AtomicBool>() + 2 * std::mem::size_of::<usize>(),
            )?,
        )?;
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
            crate::transcode::HlsSessionInfo::Live(_) => return None,
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
