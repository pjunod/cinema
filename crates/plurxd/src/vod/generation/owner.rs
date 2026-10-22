//! The one concrete ordinary-VOD writer. Generic prodrun callbacks remain
//! unaudited: this owner does not by itself mint an adaptive close receipt.
use super::*;

/// Retained together with the actual slot writer and admission, not sent through
/// a completion channel. A trailer/notification is never an asset-close fact.
pub(super) struct GenerationOutput {
    state: tokio::sync::Mutex<OutputState>,
}
struct OutputState {
    terminal: Option<GenerationTerminal>,
    // This independently opened audio/source fence supplied an inherited child
    // descriptor. Keep the real parent copy, not just its pathname or inode.
    source: Option<crate::fragment_index_cluster::SourceFence>,
}
struct GenerationTerminal {
    outcome: Option<Outcome>,
    completed: Option<(RenditionSink, bool)>,
    diagnostic: String,
}

/// Slot resources keep output custody through the ACTUAL writer join. The
/// independent metadata observer retains the same cell beyond that narrow reap.
pub(super) struct GenerationResources {
    pub(super) _admission: Option<Box<dyn Send>>,
    pub(super) _output: Arc<GenerationOutput>,
}

/// Private and single-use. Only spawn_generation constructs this concrete body;
/// callers cannot select an adapter kind or supply an arbitrary future/proof.
pub(super) struct VodGenerationOwner {
    pub(super) shared: Arc<Shared>,
    pub(super) rendition: Arc<Rendition>,
    pub(super) at: u32,
    pub(super) epoch: u64,
    pub(super) ready: tokio::sync::oneshot::Receiver<()>,
    pub(super) completed: tokio::sync::oneshot::Sender<()>,
    pub(super) landing_probe: Option<LandingProbe>,
    pub(super) output: Arc<GenerationOutput>,
}
impl VodGenerationOwner {
    pub(super) async fn run(
        self,
        registration: crate::prodrun::ProducerRegistration,
        stdout: tokio::process::ChildStdout,
        stderr: tokio::process::ChildStderr,
    ) {
        if self.ready.await.is_err()
            || self.rendition.closed.load(Relaxed)
            || self.rendition.failure().is_some()
            || self.rendition.gen_epoch.load(Relaxed) != self.epoch
            || self.rendition.cancelled_preparation_epoch.load(Acquire)
                == self.epoch.saturating_add(1)
        {
            // The acquired pipe ends close with this task; there is no terminal
            // output, but the source cell still belongs to slot/observer custody.
            let _ = self.completed.send(());
            return;
        }
        let (retiring, retired) = tokio::sync::oneshot::channel();
        // Both futures run INSIDE the registered task. No detached diagnostic
        // task or output-producing join result can escape its actual owner.
        let ((outcome, completed), diagnostic) = tokio::join!(
            async {
                let outcome = run_generation(
                    Arc::clone(&self.shared),
                    Arc::clone(&self.rendition),
                    Box::new(stdout),
                    self.at,
                    self.epoch,
                    registration.retirement(),
                    self.landing_probe,
                )
                .await;
                // Request, NEVER await this task's own reap receipt.
                let _ = super::super::driver::request_registered_driver_retirement(
                    &self.shared,
                    &self.rendition,
                    &registration,
                )
                .await;
                let _ = retiring.send(());
                outcome
            },
            async {
                let diagnostic = crate::ffmpeg::drain_diagnostics(stderr);
                tokio::pin!(diagnostic);
                tokio::select! {
                    text = &mut diagnostic => text,
                    _ = retired => match tokio::time::timeout(Duration::from_secs(5), &mut diagnostic).await {
                        Ok(text) => text,
                        Err(_) => "producer diagnostic drain exceeded its retirement deadline".to_owned(),
                    },
                }
            }
        );
        // No payload goes through oneshot. Nobody can borrow the terminal cell
        // before notification, so installation is synchronous and noncancelable.
        self.output
            .state
            .try_lock()
            .expect("unpublished VOD terminal")
            .terminal = Some(GenerationTerminal {
            outcome,
            completed,
            diagnostic,
        });
        let _ = self.completed.send(());
    }
}

impl GenerationOutput {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            state: tokio::sync::Mutex::new(OutputState {
                terminal: None,
                source: None,
            }),
        })
    }
    pub(super) fn retain_source(&self, source: Option<crate::fragment_index_cluster::SourceFence>) {
        self.state
            .try_lock()
            .expect("unpublished VOD source owner")
            .source = source;
    }

    /// Called only AFTER actual registered retirement has been joined. Borrow
    /// the terminal through each await so cancelled metadata cannot take/drop
    /// the live sink as a side effect of receiving a completion message.
    pub(super) async fn finish(
        &self,
        shared: &Arc<Shared>,
        rendition: &Arc<Rendition>,
        epoch: u64,
        at: u32,
    ) {
        let mut state = self.state.lock().await;
        let Some(terminal) = state.terminal.as_mut() else {
            // Readiness refusal has no output. It still closes its retained
            // parent source only here, after the child/writer retirement join.
            state.source.take();
            return;
        };
        if let Some((sink, _)) = terminal.completed.as_ref() {
            vodgen::Sink::completed_output(sink).await;
        }
        terminal.completed.take();
        let diagnostic = crate::ffmpeg::classify_diagnostic(&terminal.diagnostic);
        if !diagnostic.informational.is_empty() {
            tracing::debug!(target: "plurxd::vodserve", rendition = %rendition.key, generation = epoch, informational = %diagnostic.informational, "VOD producer informational output");
        }
        if !diagnostic.actionable.is_empty() {
            tracing::warn!(target: "plurxd::vodserve", rendition = %rendition.key, generation = epoch, diagnostic = %diagnostic.actionable, "VOD producer diagnostic");
        }
        if let Some(outcome) = terminal.outcome.take() {
            on_generation_end(shared, rendition, outcome, epoch, at).await;
        }
        state.terminal.take();
        state.source.take();
        // This releases local completion ownership only, NOT cached rendition
        // files, directory I/O, shared source owners, or an adaptive reservation.
    }
}

#[cfg(test)]
#[path = "owner/tests.rs"]
mod tests;
