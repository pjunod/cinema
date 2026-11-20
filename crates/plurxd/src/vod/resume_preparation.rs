//! Bounded preparation of an ordinary indexed copy rendition's resume window.
use super::*;

/// A dedicated background source budget belongs to this work until settlement.
/// Buffered output may be read by an ordinary reader as soon as its normal
/// manifest publishes it, but the background owner must await physical exit.
pub(crate) struct ResumePreparation {
    first: u32,
    last: u32,
    remaining_bytes: AtomicU64,
    deadline: Instant,
    stop: tokio_util::sync::CancellationToken,
    settled: tokio_util::sync::CancellationToken,
    source_budget: Arc<crate::background_jobs::SourceReadBudget>,
    producer: StdMutex<ResumeProducer>,
}

enum ResumeProducer {
    Unopened,
    SourceOpen,
    Registered(crate::prodrun::ProducerRegistration),
}

#[derive(Clone)]
pub(super) struct ResumePreparationDispatch {
    pub(super) work: Arc<ResumePreparation>,
    pub(super) epoch: Option<u64>,
}

impl ResumePreparation {
    pub(crate) fn cancel(&self) {
        self.stop.cancel();
        self.source_budget.cancellation().cancel();
    }

    pub(crate) async fn wait_settled(&self) {
        self.settled.cancelled().await;
    }

    pub(super) async fn wait_for_stop(&self) {
        let source_cancelled = self.source_budget.cancellation();
        tokio::select! {
            _ = self.stop.cancelled() => {},
            _ = source_cancelled.cancelled() => {},
            _ = tokio::time::sleep_until(self.deadline.into()) => {},
        }
    }

    pub(super) fn is_live(&self) -> bool {
        Instant::now() < self.deadline
            && !self.stop.is_cancelled()
            && !self.source_budget.cancellation().is_cancelled()
    }

    pub(super) fn source_budget(&self) -> Arc<crate::background_jobs::SourceReadBudget> {
        Arc::clone(&self.source_budget)
    }

    pub(super) fn source_opened(&self) {
        *self.producer.lock().expect("resume producer") = ResumeProducer::SourceOpen;
    }

    pub(super) fn unregistered_source_joined(&self) {
        *self.producer.lock().expect("resume producer") = ResumeProducer::Unopened;
    }

    pub(super) fn producer_registered(&self, registration: &crate::prodrun::ProducerRegistration) {
        *self.producer.lock().expect("resume producer") =
            ResumeProducer::Registered(registration.clone());
    }

    fn physically_settled(&self) -> bool {
        match &*self.producer.lock().expect("resume producer") {
            ResumeProducer::Unopened => true,
            ResumeProducer::SourceOpen => false,
            ResumeProducer::Registered(registration) => registration
                .confirmed_reap()
                .is_some_and(|receipt| receipt.matches(registration)),
        }
    }

    pub(super) fn next_gap(&self, manifest: &Manifest) -> Option<u32> {
        self.is_live()
            .then(|| manifest.next_gap(self.first))
            .flatten()
            .filter(|next| *next <= self.last)
    }

    pub(super) fn permits_entry(&self, entry: u32) -> bool {
        self.is_live() && (self.first..=self.last).contains(&entry)
    }

    /// Charge the bytes that this generation publishes, including its init.
    /// A failed write need not refund: this work has one finite attempt.
    pub(super) fn charge(&self, bytes: u64) -> bool {
        self.is_live()
            && self
                .remaining_bytes
                .fetch_update(AcqRel, Acquire, |remaining| remaining.checked_sub(bytes))
                .is_ok()
    }
}

impl Rendition {
    pub(super) fn queue_resume_preparation(
        &self,
        first: u32,
        last: u32,
        byte_cap: u64,
        deadline: Instant,
        source_budget: Arc<crate::background_jobs::SourceReadBudget>,
    ) -> Option<Arc<ResumePreparation>> {
        if self.ephemeral_processing
            || self.private_storage.is_some()
            || self.key.starts_with("source-")
            || self.recipe.encoding.is_some()
            || self.index.is_none()
            || self.source.is_none()
            || self.closed.load(Acquire)
            || self.failure().is_some()
            || first > last
            || self.plan.entry(last).is_none()
            || byte_cap == 0
            || deadline <= Instant::now()
            || source_budget.cancellation().is_cancelled()
            || !matches!(self.slot.try_belief(), Some(Producer::Absent { .. }))
        {
            return None;
        }
        let readers = self.readers.try_lock().ok()?;
        if !readers.is_empty() {
            return None;
        }
        let mut slot = self.resume_preparation.lock().expect("resume preparation");
        if self.closed.load(Acquire) || slot.is_some() || self.preparation().is_some() {
            return None;
        }
        let work = Arc::new(ResumePreparation {
            first,
            last,
            remaining_bytes: AtomicU64::new(byte_cap),
            deadline,
            stop: tokio_util::sync::CancellationToken::new(),
            settled: tokio_util::sync::CancellationToken::new(),
            source_budget,
            producer: StdMutex::new(ResumeProducer::Unopened),
        });
        *slot = Some(ResumePreparationDispatch {
            work: Arc::clone(&work),
            epoch: None,
        });
        drop(slot);
        drop(readers);
        self.kick();
        Some(work)
    }

    pub(super) fn resume_preparation(&self) -> Option<ResumePreparationDispatch> {
        self.resume_preparation
            .lock()
            .expect("resume preparation")
            .clone()
    }

    pub(super) fn resume_preparation_for_epoch(
        &self,
        epoch: u64,
    ) -> Option<Arc<ResumePreparation>> {
        self.resume_preparation()
            .filter(|dispatch| dispatch.epoch == Some(epoch))
            .map(|dispatch| dispatch.work)
    }

    pub(super) fn dispatch_resume_preparation(&self, work: &Arc<ResumePreparation>, epoch: u64) {
        let mut slot = self.resume_preparation.lock().expect("resume preparation");
        if let Some(active) = slot
            .as_mut()
            .filter(|active| Arc::ptr_eq(&active.work, work))
        {
            active.epoch = Some(epoch);
        }
    }

    /// Call only if this work never opened a producer, or after its exact
    /// registered producer reaper has joined the child, writer and input.
    pub(super) fn settle_resume_preparation(&self, work: &Arc<ResumePreparation>) -> bool {
        if !work.physically_settled() {
            return false;
        }
        let mut slot = self.resume_preparation.lock().expect("resume preparation");
        if slot
            .as_ref()
            .is_some_and(|active| Arc::ptr_eq(&active.work, work))
        {
            *slot = None;
            work.cancel();
            work.settled.cancel();
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn prepared_sink(
        base: &Path,
        byte_cap: u64,
    ) -> (
        super::super::generation::RenditionSink,
        Arc<ResumePreparation>,
    ) {
        let serve = super::super::tests::bare_serve(base);
        let rendition = super::super::tests::synthetic_rendition(base).await;
        // The fixture tests the sink only. This budget never opens a source
        // or confers a grant on the synthetic rendition.
        let source_budget = crate::background_jobs::SourceReadBudget::new(
            Arc::clone(&serve.shared.store),
            plurx_core::store::background_jobs_resources::BackgroundSourceOwner::Analysis {
                request_id: "resume-sink-fixture".to_owned(),
                node_id: "fixture".to_owned(),
                fence: 1,
            },
            tokio_util::sync::CancellationToken::new(),
        );
        let work = Arc::new(ResumePreparation {
            first: 2,
            last: 3,
            remaining_bytes: AtomicU64::new(byte_cap),
            deadline: Instant::now() + Duration::from_secs(10),
            stop: tokio_util::sync::CancellationToken::new(),
            settled: tokio_util::sync::CancellationToken::new(),
            source_budget,
            producer: StdMutex::new(ResumeProducer::Unopened),
        });
        *rendition.resume_preparation.lock().expect("test dispatch") =
            Some(ResumePreparationDispatch {
                work: Arc::clone(&work),
                epoch: Some(0),
            });
        let sink = super::super::generation::RenditionSink {
            shared: Arc::clone(&serve.shared),
            rendition,
            epoch: 0,
            retirement: tokio_util::sync::CancellationToken::new(),
        };
        (sink, work)
    }

    #[tokio::test]
    async fn resume_sink_fences_entries_outside_prepared_window() {
        let base = crate::test_tempdir().expect("resume output fixture");
        let (sink, work) = prepared_sink(base.path(), 100).await;
        let manifest = sink.rendition.manifest.lock().await;
        assert_eq!(work.next_gap(&manifest), Some(2));
        drop(manifest);
        vodgen::Sink::materialize(&sink, 2, vec![1; 20])
            .await
            .expect("first prepared fragment");
        vodgen::Sink::materialize(&sink, 3, vec![1; 20])
            .await
            .expect("last prepared fragment");
        let manifest = sink.rendition.manifest.lock().await;
        assert_eq!(work.next_gap(&manifest), None);
        drop(manifest);
        assert_eq!(
            vodgen::Sink::materialize(&sink, 4, vec![1; 20])
                .await
                .expect_err("late producer output cannot escape the window")
                .kind(),
            io::ErrorKind::NotFound
        );
        let manifest = sink.rendition.manifest.lock().await;
        assert_eq!(manifest.materialized_count(), 2);
        assert!(sink.rendition.failure().is_none());
    }

    #[tokio::test]
    async fn resume_sink_refuses_bytes_past_its_budget_without_poisoning_rendition() {
        let base = crate::test_tempdir().expect("resume byte limit fixture");
        let (sink, work) = prepared_sink(base.path(), 32).await;
        vodgen::Sink::materialize(&sink, 2, vec![1; 24])
            .await
            .expect("fragment fits the budget");
        assert_eq!(
            vodgen::Sink::materialize(&sink, 3, vec![1; 24])
                .await
                .expect_err("next fragment exceeds the budget")
                .kind(),
            io::ErrorKind::NotFound
        );
        let manifest = sink.rendition.manifest.lock().await;
        assert_eq!(manifest.materialized_bytes(), 24);
        assert!(!work.is_live());
        assert!(sink.rendition.failure().is_none());
        assert!(
            !work.settled.is_cancelled(),
            "stop is not an acknowledgement of physical retirement"
        );
    }
    struct EvictableFile(PathBuf);

    #[async_trait::async_trait]
    impl crate::playback_cache::CacheOwner for EvictableFile {
        async fn candidates(&self) -> io::Result<Vec<crate::playback_cache::CacheCandidate>> {
            Ok(vec![crate::playback_cache::CacheCandidate {
                path: self.0.clone(),
                last_used: std::time::SystemTime::UNIX_EPOCH,
            }])
        }

        async fn evict(
            &self,
            candidate: &crate::playback_cache::CacheCandidate,
        ) -> io::Result<bool> {
            tokio::fs::remove_file(&candidate.path).await?;
            Ok(true)
        }
    }

    #[tokio::test]
    async fn oversized_resume_fragment_cannot_evict_unrelated_cache_bytes() {
        let base = crate::test_tempdir().expect("resume cache fixture");
        let (sink, work) = prepared_sink(base.path(), 8).await;
        let unrelated = base.path().join("unrelated.mp4");
        tokio::fs::write(&unrelated, vec![0x5a; 16 << 10])
            .await
            .expect("unrelated cached media");
        let budget = crate::playback_cache::PlaybackCacheBudget::new(
            Arc::clone(&sink.shared.store),
            vec![base.path().to_path_buf()],
        );
        budget.register(Arc::new(EvictableFile(unrelated.clone())));
        let used = budget.used_bytes().await.expect("cache inventory");
        assert!(used >= 24);
        budget.configure_for_test(used);

        let error = vodgen::Sink::materialize(&sink, 2, vec![1; 24])
            .await
            .expect_err("oversized preparation must not ask the cache to free space");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert!(!work.is_live());
        assert!(unrelated.exists());

        let _ordinary_reservation = crate::playback_cache::reserve(sink.rendition.dir.path(), 24)
            .await
            .expect("ordinary playback can reserve space");
        assert!(!unrelated.exists(), "the fixture had real cache pressure");
    }
}
