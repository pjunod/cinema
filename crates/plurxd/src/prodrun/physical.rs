//! Slot operations against either its legacy child or a core-owned graph root.
//! The physical variant never exports the child/job or interprets a producer
//! receipt as permission to refund the whole graph.

use super::*;
use plurx_core::adaptive_resources::physical_graph::{
    GraphCloseOutcome, GraphHoldReason, GraphRoot, SealedPhysicalGraph,
};

pub(super) enum ProducerChild {
    Legacy(Child),
    #[cfg_attr(not(unix), allow(dead_code))]
    Physical {
        graph: SealedPhysicalGraph,
        root: GraphRoot,
    },
}

#[cfg(unix)]
impl EmptyProducerSlot {
    /// Launch/register under the already-reserved slot. Core owns the actual
    /// physical reservation BEFORE launch and the actual writer JoinHandle.
    /// Adapters must not detach work or wait for their OWN reap receipt.
    /// Production policy/adapters still need to supply these reservations.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) async fn spawn_registered_custodied<F, Fut>(
        mut self,
        program: &std::path::Path,
        args: &[String],
        options: crate::producer_spawn::SpawnOptions<'_>,
        reservation: plurx_core::adaptive_resources::ResourceReservation,
        at: u32,
        writer: F,
    ) -> Result<ProducerRegistration, String>
    where
        F: FnOnce(
            ProducerRegistration,
            plurx_core::adaptive_resources::physical_graph::RootPipes,
        ) -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let crate::producer_spawn::CustodiedSpawned { mut graph, root } =
            crate::producer_spawn::spawn_custodied(program, args, options, reservation).await?;
        // No sender barrier: core joins the actual task before the event.
        let registration = new_registration(None);
        let writer_registration = registration.clone();
        graph
            .spawn_root_writer(root, |pipes| writer(writer_registration, pipes))
            .await
            .map_err(|error| format!("registering custodied producer writer: {error}"))?;
        self.0.child = Some(ProducerChild::Physical {
            graph: graph.seal(),
            root,
        });
        self.0.registration = Some(registration.clone());
        self.0.belief = after(self.0.belief, Step::Start { at });
        Ok(registration)
    }
}

impl ProducerChild {
    pub(super) async fn start_kill(&mut self) -> io::Result<()> {
        match self {
            Self::Legacy(child) => child.start_kill(),
            Self::Physical { graph, root } => graph.start_kill_root(*root).await,
        }
    }

    pub(super) async fn wait(
        &mut self,
        hooks: &dyn ProducerReapHooks,
    ) -> io::Result<std::process::ExitStatus> {
        match self {
            Self::Legacy(child) => hooks.wait(child).await,
            Self::Physical { graph, root } => graph.wait_root(*root).await,
        }
    }

    pub(super) async fn try_wait(&mut self) -> io::Result<Option<std::process::ExitStatus>> {
        match self {
            Self::Legacy(child) => child.try_wait(),
            Self::Physical { graph, root } => graph.try_wait_root(*root).await,
        }
    }

    pub(super) async fn signal(
        &self,
        signal: crate::process_control::ProcessSignal,
    ) -> io::Result<bool> {
        match self {
            Self::Legacy(child) => {
                crate::process_control::signal(child.id().ok_or_else(no_child)?, signal)
            }
            Self::Physical { graph, root } => graph.signal_root(*root, signal).await,
        }
    }

    /// Successful return proves the owned roots and actual registered tasks
    /// ended, NOT whole-resource/descendant extinction. That remains core's
    /// UnknownHeld until a supported containment/resource backend proves it.
    pub(super) async fn join_graph_writers(&self) -> io::Result<WriterSettlement> {
        let Self::Physical { graph, .. } = self else {
            return Ok(WriterSettlement::Settled);
        };
        match graph.close().await {
            GraphCloseOutcome::UnknownHeld(GraphHoldReason::ContainmentUnavailable) => {
                Ok(if graph.snapshot().await.abandoned_writers == 0 {
                    WriterSettlement::Settled
                } else {
                    WriterSettlement::Abandoned
                })
            }
            // No-acquisition is impossible for an attached launched root. A
            // broken invariant or failed wait must not mint a producer receipt.
            outcome => Err(io::Error::other(format!(
                "producer graph closure unfinished: {outcome:?}"
            ))),
        }
    }
}

#[cfg(all(test, unix))]
mod tests;
