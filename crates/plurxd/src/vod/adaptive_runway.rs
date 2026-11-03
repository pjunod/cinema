//! Owner-local evidence for a possible VOD incumbent trial guard.
//!
//! This read cannot authorize a trial. The controller must still enumerate
//! every affected incumbent, recheck current ownership, and have verified
//! startup, cleanup and memory bounds before it asks the policy to allocate.
use super::*;

const POLICY_REPORT_FRESHNESS: Duration = Duration::from_secs(15);

#[derive(Clone, Debug)]
pub(crate) struct AcceptedVodRunway {
    pub generation: uuid::Uuid,
    pub owner_epoch: u64,
    pub accepted_sequence: u64,
    pub observed_at: Instant,
    pub produced_frontier_ms: i64,
    pub runway: Duration,
}

impl VodServe {
    /// Read the latest accepted client buffer and the contiguous server
    /// publication frontier for the SAME session incarnation and sequence.
    /// Missing contiguity, stale reports, a changed owner or a control race
    /// returns Unknown (`None`) rather than permission to explore.
    pub(crate) async fn accepted_policy_runway(
        &self,
        session_id: &str,
        expected_generation: uuid::Uuid,
        expected_owner_epoch: u64,
    ) -> Option<AcceptedVodRunway> {
        let before = {
            let sessions = self.shared.sessions.lock().await;
            let session = sessions.get(session_id)?;
            if session.tombstone.is_some() {
                return None;
            }
            let control = session.control.lock().ok()?;
            let (generation, epoch, sequence, accepted_at) = control.accepted_policy_identity()?;
            if generation != expected_generation || epoch != expected_owner_epoch {
                return None;
            }
            let observed_at = session.control_observed_at?;
            (sequence, accepted_at, observed_at)
        };

        // status_publication uses the accepted snapshot's anchor and walks
        // only materialized consecutive entries. A seek-created island past
        // a hole cannot become an invented produced frontier here.
        let publication = self.status_publication(session_id).await?;
        let status = publication.result.ok()?;
        let sessions = self.shared.sessions.lock().await;
        let session = sessions.get(session_id)?;
        if session.tombstone.is_some()
            || !Arc::ptr_eq(&session.lifecycle, &publication.owner.lifecycle)
            || !Arc::ptr_eq(&session.incarnation, &publication.owner.incarnation)
            || session.rendition_key != publication.owner.rendition_key
            || !session.owns_response_media(&publication.owner)
        {
            return None;
        }
        let control = session.control.lock().ok()?;
        let (generation, epoch, sequence, accepted_at) = control.accepted_policy_identity()?;
        if generation != expected_generation || epoch != expected_owner_epoch {
            return None;
        }
        let observed_at = session.control_observed_at?;
        if (sequence, accepted_at, observed_at) != before || observed_at < accepted_at {
            return None;
        }
        let age = Instant::now().checked_duration_since(observed_at)?;
        if age > POLICY_REPORT_FRESHNESS {
            return None;
        }
        let snapshot = session.last_control_snapshot.as_ref()?;
        if snapshot.demand != crate::playback_control::PlaybackDemand::Active
            || snapshot.render_state != crate::playback_control::RenderState::Rendering
            || snapshot.seek_target_ms.is_some()
            || status.server_ready_state != "ready"
            || status.reported_position_ms != Some(snapshot.position_ms)
        {
            return None;
        }
        let anchor = snapshot.buffer_anchor_ms();
        if snapshot.buffered_from_ms.is_none_or(|start| start > anchor)
            || snapshot.buffered_through_ms < anchor
            || status.server_ready_anchor_ms != Some(anchor)
        {
            return None;
        }
        let produced_frontier_ms = status.server_ready_end_ms?;
        let safe_end = produced_frontier_ms.min(snapshot.buffered_through_ms);
        let media_ms = u64::try_from(safe_end.checked_sub(anchor)?).ok()?;
        // Round the client's rate upward before dividing, so conversion to
        // wall time never credits extra runway through fractional rounding.
        let rate_milli = (snapshot.playback_rate * 1000.0).ceil() as u64;
        if !(250..=4000).contains(&rate_milli) {
            return None;
        }
        let wall_ms = media_ms.checked_mul(1000)?.checked_div(rate_milli)?;
        let runway = Duration::from_millis(wall_ms).checked_sub(age)?;
        Some(AcceptedVodRunway {
            generation: expected_generation,
            owner_epoch: expected_owner_epoch,
            accepted_sequence: sequence,
            observed_at,
            produced_frontier_ms,
            runway,
        })
    }
}
