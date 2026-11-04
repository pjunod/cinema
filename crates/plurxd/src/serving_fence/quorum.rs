//! Optional clustered readiness provider. Local admission never constructs it.
//!
//! Quorum leases and retained proofs retain their existing deadlines. This
//! adapter supplies readiness to the common admission/drain implementation.

use std::sync::{Arc, Mutex as StdMutex};

use plurx_core::cluster::migration::status::{PassiveRaftMetrics, ServingProof};

use super::{decide_authority, Retain, ServingFence, ServingReadinessSource};

struct QuorumReadiness {
    metrics: PassiveRaftMetrics,
    retained_proof: StdMutex<Option<ServingProof>>,
}

impl ServingFence {
    /// Compatibility boundary for clustered callers. Native callers use
    /// `local()` and never construct passive metrics or a provider.
    pub(crate) fn new(metrics: PassiveRaftMetrics) -> Self {
        if !metrics.snapshot().watermark_source {
            return Self::local();
        }
        Self::with_readiness_source(Some(Arc::new(QuorumReadiness {
            metrics,
            retained_proof: StdMutex::new(None),
        })))
    }
}

impl QuorumReadiness {
    /// Whether serving may continue on the proof this node already held.
    ///
    /// A fallback, reached only when the current snapshot is not eligible, and
    /// only after that snapshot has been given its chance — a new eligible
    /// proof is always preferred and always replaces the retained one.
    ///
    /// The retained proof keeps its *original* deadline. Nothing here rewrites
    /// it: a catch-up does not restamp it, the newer watermark that caused
    /// this path does not extend it, and a sampling error does not refresh it.
    /// So the window is strictly no longer than the one this node already had,
    /// and strictly shorter than the newer proof's, which means an isolated
    /// node still loses authority at exactly the instant it does today. What
    /// changes is only this: a node that is *in* the quorum, holding a live
    /// proof, no longer loses serving the moment a newer committed index
    /// arrives ahead of its local apply — which is a self-inflicted outage on
    /// a node nothing is actually wrong with.
    ///
    /// Every other reason to drop it is still a hard drop, checked against
    /// live state on each poll rather than trusted from capture time: the
    /// original expiry, an explicit invalidation, a term or leader or epoch
    /// that no longer matches the generation the proof was issued in, a stale
    /// local sample, a lost leader, a regressed committed index.
    fn continues_on_the_retained_proof(&self) -> bool {
        let mut retained = self.retained_proof.lock().unwrap_or_else(|poisoned| {
            // A poisoned lock is a panic somewhere in this poller. Fail closed
            // rather than serve on a proof whose provenance is now in doubt.
            poisoned.into_inner()
        });
        let Some(proof) = *retained else {
            return false;
        };
        if self.metrics.serving_proof_remains_valid(&proof) {
            return true;
        }
        // Cleared on the way out, so a proof that has died cannot be revived
        // by anything that happens later.
        *retained = None;
        false
    }

    fn retain(&self, proof: Option<ServingProof>) {
        *self
            .retained_proof
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = proof;
    }

    fn ready_inner(&self, previous: bool) -> bool {
        let snapshot = self.metrics.snapshot();
        // On a locally bound node, any live confirmation of the current
        // generation that this node has applied is current authority, not
        // only the newest one at zero lag (rule 4, amended). Read once, so
        // the proof captured below is the one that made this poll ready.
        let applied_proof = snapshot
            .watermark_requires_local_binding
            .then(|| self.metrics.eligible_serving_proof())
            .flatten();
        let authority_is_current = applied_proof.is_some()
            || (snapshot.watermark_valid
                && snapshot.watermark.is_some_and(|watermark| {
                    if snapshot.watermark_requires_local_binding {
                        snapshot.local_source && watermark.apply_lag_entries == Some(0)
                    } else {
                        !snapshot.local_source && watermark.apply_lag_entries.is_none()
                    }
                }));
        let outcome = decide_authority(
            false,
            authority_is_current,
            previous,
            // Evaluated lazily: the retained proof is only consulted on the
            // one branch that may use it, so an eligible snapshot never pays
            // for it and a lost node never resurrects one by reading it.
            || self.continues_on_the_retained_proof(),
        );
        match outcome.retain {
            Retain::FromCurrentProof => {
                self.retain(applied_proof.or_else(|| self.metrics.eligible_serving_proof()))
            }
            Retain::Clear => self.retain(None),
            Retain::Unchanged => {}
        }
        outcome.ready
    }
}

impl ServingReadinessSource for QuorumReadiness {
    fn ready(&self, previously_ready: bool) -> bool {
        self.ready_inner(previously_ready)
    }

    fn loss_observed(&self, next_generation: u64) {
        tracing::warn!(attempt = ?self.metrics.quorum_attempt(),
            proof = ?self.metrics.snapshot(), loss_generation = next_generation,
            "serving authority expiring; in-flight quorum evidence");
    }

    fn published(&self, ready: bool) {
        if ready {
            tracing::info!("serving authority recovered from a fresh quorum watermark");
        } else {
            tracing::warn!("serving authority expired; mutable media is self-fenced");
        }
    }
}
