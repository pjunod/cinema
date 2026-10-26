//! Positive LIVE process-only evidence from original retained Windows Jobs.
//! This deliberately does not certify tasks, source handles, external work,
//! durable settlement, a daemon restart or a reboot. No PhysicalCloseReceipt.
use super::*;

/// Nonclone/nonserde fact retained by core, bound to one sealed graph. It cannot
/// authorize a refund or be reconstructed from caller-supplied audit fields.
#[derive(Debug)]
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "Original Windows Job whole-process proof is retained for the unconnected legacy live adapter; fixed Linux adapters use their own bound closures."
    )
)]
pub(in crate::adaptive_resources) struct LiveProcessClosure {
    binding: ReservationBinding,
    generation: Uuid,
    roots: Vec<GraphRoot>,
}

impl LiveProcessClosure {
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Legacy whole-graph Windows Job closure has fixture callers only; production finite/copy/planning settle through dedicated typed adapters."
        )
    )]
    pub(in crate::adaptive_resources) fn matches(&self, graph: &SealedPhysicalGraph) -> bool {
        self.binding == graph.custody.binding
            && self.generation == graph.custody.identity.generation
            && graph.custody.sealed.load(Ordering::Acquire)
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Legacy whole-graph Windows Job closure has fixture callers only; production finite/copy/planning settle through dedicated typed adapters."
        )
    )]
    pub(in crate::adaptive_resources) fn roots(&self) -> &[GraphRoot] {
        &self.roots
    }
}

/// Best-effort cleanup visits every retained original Job. Failure of one Job
/// must not prevent requesting termination of the rest. A later native query,
/// not this request result or root PID absence, determines process extinction.
pub(super) fn request_termination(members: &Members) {
    for root in &members.roots {
        if let Some(job) = &root.job {
            let _ = job.terminate_containment();
        }
    }
}

impl SealedPhysicalGraph {
    /// Query once, under the sealed graph's member lock, after real root waits.
    /// Existing supervised cleanup can retry WouldBlock/error; there is no
    /// timeout release, busy loop, new watcher or replacement empty Job here.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Legacy whole-graph Windows Job closure has fixture callers only; production finite/copy/planning settle through dedicated typed adapters."
        )
    )]
    pub(in crate::adaptive_resources) async fn try_live_process_closure(
        &self,
    ) -> io::Result<LiveProcessClosure> {
        let members = self.custody.members.lock().await;
        if !self.custody.sealed.load(Ordering::Acquire)
            || members.uncertain_launch
            || members.roots.is_empty()
        {
            return Err(io::Error::other(
                "sealed complete root inventory unavailable",
            ));
        }
        let mut roots = Vec::with_capacity(members.roots.len());
        for (index, root) in members.roots.iter().enumerate() {
            if !root.waited {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "original root has not been waited",
                ));
            }
            let job = root
                .job
                .as_ref()
                .ok_or_else(|| io::Error::other("original root Job unavailable"))?;
            if job.containment_population()? != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "original Job still has active processes",
                ));
            }
            roots.push(GraphRoot {
                generation: self.custody.identity.generation,
                index,
                process: root.process,
                control: job.registered_control(),
            });
        }
        // Every object remains retained, no new root can enter, and these
        // unnamed Job handles are never exported. Zero is therefore monotonic
        // for this sealed live object set; it says nothing about a restart.
        Ok(LiveProcessClosure {
            binding: self.custody.binding,
            generation: self.custody.identity.generation,
            roots,
        })
    }
}

#[cfg(test)]
mod tests;
