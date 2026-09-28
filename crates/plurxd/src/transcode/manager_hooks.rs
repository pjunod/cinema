use super::*;

/// The points of the [`TranscodeManager`] that a race test can pause at, and
/// the ones it can answer or record (TRANSCODE-DECOMPOSITION-PLAN §3.9, M8).
///
/// The manager holds these in every build, so its layout and the await points
/// of the paths below are the same in the test and release binaries. `Any`
/// is a supertrait only so a test can reach the test hooks behind the
/// manager.
pub(crate) trait TranscodeManagerHooks: std::any::Any + Send + Sync {
    /// Pause: a subtitle playlist's bytes and exact owner are resolved and its
    /// cues are warm, before the composite response commits.
    fn before_subtitle_playlist_commit(&self) -> crate::seam_hooks::HookFuture<'static>;
    /// Pause: a VOD response's publication was admitted by the serving
    /// authority, before it awaits its owner.
    fn after_vod_publication_admission(&self) -> crate::seam_hooks::HookFuture<'static>;
    /// Override: whether plans resolve under the published artifact identity
    /// without per-path contract selection. Production: never.
    fn forces_artifact_qualification(&self) -> bool;
    /// Record: the real artifact-qualification publisher ran; it decides the
    /// identity from here on, so a forced one ends.
    fn artifact_qualification_published(&self);
    /// Override: the outcome of an offline production of `recipe` in place of
    /// producing it. Production: `None`, so the recipe is produced.
    fn scripted_offline_outcome(&self, recipe: &str) -> Option<OfflineProduceOutcome>;
    /// Fault: the error the offline recovery-budget write fails with in place
    /// of the Store. Production: `None`, so the Store is written.
    fn offline_recovery_begin_fault(&self) -> Option<plurx_core::error::StoreError>;
    /// Record: a generation manifest was published.
    fn manifest_published(&self);
    /// Plumbing: the decode-fact source a plan resolution probes through.
    /// Production returns it unchanged; a test hands it a delay for its own
    /// hooks (`DecodeFactSource`'s, migrated in #556).
    fn decode_fact_source(
        &self,
        source: crate::decode_facts::DecodeFactSource,
    ) -> crate::decode_facts::DecodeFactSource;
}

/// What production installs: every point is ready, nothing is overridden or
/// injected, and nothing is recorded.
pub(crate) struct NoopTranscodeManagerHooks;

impl TranscodeManagerHooks for NoopTranscodeManagerHooks {
    fn before_subtitle_playlist_commit(&self) -> crate::seam_hooks::HookFuture<'static> {
        Box::pin(crate::seam_hooks::HookReady)
    }

    fn after_vod_publication_admission(&self) -> crate::seam_hooks::HookFuture<'static> {
        Box::pin(crate::seam_hooks::HookReady)
    }

    fn forces_artifact_qualification(&self) -> bool {
        false
    }

    fn artifact_qualification_published(&self) {}

    fn scripted_offline_outcome(&self, _: &str) -> Option<OfflineProduceOutcome> {
        None
    }

    fn offline_recovery_begin_fault(&self) -> Option<plurx_core::error::StoreError> {
        None
    }

    fn manifest_published(&self) {}

    fn decode_fact_source(
        &self,
        source: crate::decode_facts::DecodeFactSource,
    ) -> crate::decode_facts::DecodeFactSource {
        source
    }
}

/// The manager's test hooks.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct TranscodeManagerTestHooks {
    pub(crate) subtitle_playlist_commit: crate::seam_hooks::PauseSlot,
    pub(crate) vod_publication_admission: crate::seam_hooks::PauseSlot,
    /// Tests that exercise receipt enforcement without installing a real
    /// diagnostic policy can explicitly force the old all-plan identity. The
    /// production path is always contract-scoped.
    pub(super) force_artifact_qualification: std::sync::atomic::AtomicBool,
    /// Deterministic offline coordinator outcomes, and the exact recipe
    /// reference each scripted attempt received. Production always enters
    /// `produce_normalized`; tests use this only to prove the durable
    /// primary-to-alternate transition without depending on host hardware.
    pub(super) offline_produce_script:
        std::sync::Mutex<std::collections::VecDeque<OfflineProduceOutcome>>,
    pub(super) offline_produced_recipes: std::sync::Mutex<Vec<String>>,
    /// Inject the authority-write failure at the exact primary-fault boundary.
    pub(super) fail_next_offline_recovery_begin: std::sync::atomic::AtomicBool,
    /// How many generation manifests this manager has published.
    ///
    /// The only durable trace a manifest leaves on a refused generation is the
    /// generation itself, and a refusal quarantines that — so without this
    /// counter no test can tell a manifest that was written and thrown away
    /// from one that was never written, and the milestone's central change
    /// would revert green.
    pub(super) manifests_published: std::sync::atomic::AtomicUsize,
    pub(super) decode_source_final_identity_delay: std::sync::Mutex<Option<Duration>>,
}

#[cfg(test)]
impl TranscodeManagerHooks for TranscodeManagerTestHooks {
    fn before_subtitle_playlist_commit(&self) -> crate::seam_hooks::HookFuture<'static> {
        self.subtitle_playlist_commit.hold()
    }

    fn after_vod_publication_admission(&self) -> crate::seam_hooks::HookFuture<'static> {
        self.vod_publication_admission.hold()
    }

    fn forces_artifact_qualification(&self) -> bool {
        self.force_artifact_qualification
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    fn artifact_qualification_published(&self) {
        self.force_artifact_qualification
            .store(false, std::sync::atomic::Ordering::Relaxed);
    }

    fn scripted_offline_outcome(&self, recipe: &str) -> Option<OfflineProduceOutcome> {
        let scripted = self
            .offline_produce_script
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_front()?;
        self.offline_produced_recipes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(recipe.to_owned());
        Some(scripted)
    }

    fn offline_recovery_begin_fault(&self) -> Option<plurx_core::error::StoreError> {
        self.fail_next_offline_recovery_begin
            .swap(false, std::sync::atomic::Ordering::SeqCst)
            .then(|| {
                plurx_core::error::StoreError::Database(
                    "injected offline recovery-begin failure".to_owned(),
                )
            })
    }

    fn manifest_published(&self) {
        self.manifests_published
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    fn decode_fact_source(
        &self,
        source: crate::decode_facts::DecodeFactSource,
    ) -> crate::decode_facts::DecodeFactSource {
        match *self
            .decode_source_final_identity_delay
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            Some(delay) => source.with_final_identity_delay(delay),
            None => source,
        }
    }
}

#[cfg(test)]
impl TranscodeManager {
    /// The manager's test hooks, installed on first use.
    pub(crate) fn test_hooks(&self) -> &TranscodeManagerTestHooks {
        let hooks: &dyn std::any::Any = self
            .hooks
            .get_or_install(|| Box::new(TranscodeManagerTestHooks::default()));
        hooks
            .downcast_ref()
            .expect("the manager's hook slot holds TranscodeManagerTestHooks")
    }
}
