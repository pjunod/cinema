//! The copy parser's only native publication adapter. It moves each bounded
//! payload into the already admitted ScratchAssetsV3 member and waits for the
//! same journal-observed native cell before exposing the playlist revision.
use super::*;
use crate::copy_segmenter::{CopyPublicationSink, Limits, PublishError};
use crate::fmp4::{self, Init, Published};
use crate::resource_custody::codec_v2::scratch::OutputKey;
use std::io;
use std::sync::Mutex;
use std::time::Duration;

const MAX_PLAYLIST_ENTRY_BYTES: usize = 64;
const MAX_SINK_MEMORY_BYTES: usize = 16 * 1024 * 1024;

pub(in crate::adaptive_resources) struct CopySink {
    runtime: Arc<NodeResourceRuntime>,
    preparation: OwnedPreparation,
    member: Uuid,
    max_segments: u32,
    max_payload_bytes: usize,
    reserved_memory_bytes: usize,
    gate_seconds: u32,
    target_seconds: u32,
    operation_timeout: Duration,
    entries: String,
    produced_playlist: Arc<Mutex<String>>,
    served_playlist: Arc<Mutex<String>>,
    publication_gate: Arc<tokio::sync::Mutex<()>>,
    progress: Arc<tokio::sync::Notify>,
    playlist_capacity: usize,
    published_seconds: f64,
    started: bool,
    #[cfg(all(test, feature = "fixtures"))]
    fixture_panic_next_publication: bool,
}

/// Allocated only after the exact typed R is admitted. Binding moves these
/// charged allocations into the registered original owner.
pub(in crate::adaptive_resources::runtime) struct PreparedCopySink {
    max_segments: u32,
    max_payload_bytes: usize,
    reserved_memory_bytes: usize,
    gate_seconds: u32,
    target_seconds: u32,
    operation_timeout: Duration,
    entries: String,
    produced_playlist: Arc<Mutex<String>>,
    served_playlist: Arc<Mutex<String>>,
    publication_gate: Arc<tokio::sync::Mutex<()>>,
    progress: Arc<tokio::sync::Notify>,
    playlist_capacity: usize,
}

impl CopySink {
    pub(in crate::adaptive_resources::runtime) fn maximum_admissible_segments(
        limits: Limits,
    ) -> u32 {
        let mut lower = 0u32;
        let mut upper = 65_536u32;
        while lower < upper {
            let middle = lower + (upper - lower).div_ceil(2);
            if Self::requested_memory_bytes(middle, limits)
                .is_some_and(|bytes| bytes <= MAX_SINK_MEMORY_BYTES)
            {
                lower = middle;
            } else {
                upper = middle - 1;
            }
        }
        lower
    }
    pub(in crate::adaptive_resources::runtime) fn playlist_capacity_for(
        max_segments: u32,
        limits: Limits,
    ) -> Option<usize> {
        usize::try_from(max_segments)
            .ok()?
            .checked_mul(MAX_PLAYLIST_ENTRY_BYTES)?
            .checked_add(fmp4::playlist_header(limits.target_seconds).len())?
            .checked_add("#EXT-X-ENDLIST\n".len())
    }
    pub(in crate::adaptive_resources::runtime) fn requested_memory_bytes(
        max_segments: u32,
        limits: Limits,
    ) -> Option<usize> {
        let entries = usize::try_from(max_segments)
            .ok()?
            .checked_mul(MAX_PLAYLIST_ENTRY_BYTES)?;
        let playlist = Self::playlist_capacity_for(max_segments, limits)?;
        // Produced and served revisions coexist. One candidate String and
        // its native queue Vec overlap them during a served replacement. The
        // parser's segment and native queue have separate graph demand.
        std::mem::size_of::<Self>()
            .checked_add(entries)?
            .checked_add(playlist.checked_mul(4)?)?
            .checked_add(
                (std::mem::size_of::<Mutex<String>>() + 2 * std::mem::size_of::<usize>())
                    .checked_mul(2)?,
            )
    }

    pub(in crate::adaptive_resources::runtime) fn prepare(
        max_segments: u32,
        max_payload_bytes: usize,
        limits: Limits,
        operation_timeout: Duration,
        progress: Arc<tokio::sync::Notify>,
    ) -> io::Result<PreparedCopySink> {
        let total = Self::requested_memory_bytes(max_segments, limits)
            .ok_or_else(|| io::Error::other("copy playlist memory overflow"))?;
        if max_segments == 0
            || max_segments > 65_536
            || max_payload_bytes == 0
            || operation_timeout.is_zero()
            || operation_timeout > Duration::from_secs(120)
            || total > MAX_SINK_MEMORY_BYTES
        {
            return Err(io::Error::other(
                "copy sink exceeds its bounded fixed profile",
            ));
        }
        let entries_capacity = usize::try_from(max_segments)
            .ok()
            .and_then(|n| n.checked_mul(MAX_PLAYLIST_ENTRY_BYTES))
            .ok_or_else(|| io::Error::other("copy playlist entry capacity overflow"))?;
        let playlist_capacity = Self::playlist_capacity_for(max_segments, limits)
            .ok_or_else(|| io::Error::other("copy playlist capacity overflow"))?;
        let mut entries = String::new();
        entries
            .try_reserve_exact(entries_capacity)
            .map_err(io::Error::other)?;
        let mut produced_playlist_value = String::new();
        produced_playlist_value
            .try_reserve_exact(playlist_capacity)
            .map_err(io::Error::other)?;
        let mut served_playlist_value = String::new();
        served_playlist_value
            .try_reserve_exact(playlist_capacity)
            .map_err(io::Error::other)?;
        if std::mem::size_of::<Self>()
            .checked_add(entries.capacity())
            .and_then(|n| n.checked_add(produced_playlist_value.capacity()))
            .and_then(|n| n.checked_add(served_playlist_value.capacity()))
            .and_then(|n| {
                n.checked_add(
                    (std::mem::size_of::<Mutex<String>>() + 2 * std::mem::size_of::<usize>())
                        .checked_mul(2)?,
                )
            })
            .is_none_or(|actual| actual > total)
        {
            return Err(io::Error::other(
                "copy sink allocation exceeded admitted layout",
            ));
        }
        Ok(PreparedCopySink {
            max_segments,
            max_payload_bytes,
            reserved_memory_bytes: total,
            gate_seconds: limits.publish_gate_secs,
            target_seconds: limits.target_seconds.max(1),
            operation_timeout,
            entries,
            produced_playlist: Arc::new(Mutex::new(produced_playlist_value)),
            served_playlist: Arc::new(Mutex::new(served_playlist_value)),
            publication_gate: Arc::new(tokio::sync::Mutex::new(())),
            progress,
            playlist_capacity,
        })
    }

    pub(in crate::adaptive_resources) fn reserved_memory_bytes(&self) -> usize {
        self.reserved_memory_bytes
    }

    pub(in crate::adaptive_resources::runtime) fn produced_playlist_handle(
        &self,
    ) -> Arc<Mutex<String>> {
        Arc::clone(&self.produced_playlist)
    }
    pub(in crate::adaptive_resources::runtime) fn served_playlist_handle(
        &self,
    ) -> Arc<Mutex<String>> {
        Arc::clone(&self.served_playlist)
    }
    pub(in crate::adaptive_resources::runtime) fn publication_gate_handle(
        &self,
    ) -> Arc<tokio::sync::Mutex<()>> {
        Arc::clone(&self.publication_gate)
    }

    #[cfg(all(test, feature = "fixtures"))]
    pub(in crate::adaptive_resources::runtime) fn fixture_panic_next_publication(&mut self) {
        self.fixture_panic_next_publication = true;
    }

    async fn publish(&self, output: OutputKey, bytes: Vec<u8>) -> io::Result<()> {
        #[cfg(all(test, feature = "fixtures"))]
        assert!(
            !self.fixture_panic_next_publication,
            "fixture panic in ORIGINAL copy publication sink"
        );
        if bytes.is_empty() || bytes.len() > self.max_payload_bytes {
            return Err(io::Error::other(
                "copy output exceeds admitted native payload",
            ));
        }
        if matches!(&output, OutputKey::Segment { .. }) {
            // Register before checking the original fold. Once a bounded
            // resident window is full, holding the stdout parser here lets
            // its pipe naturally backpressure FFmpeg until a real reader and
            // grace-safe RemoveOwned transition makes room. No timer, path
            // probe, second producer or uncharged spool is introduced.
            loop {
                let notified = self.progress.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                let publication = self.publication_gate.lock().await;
                let ready = self
                    .runtime
                    .copy_segment_slot_ready(
                        &self.preparation,
                        0,
                        self.member,
                        u64::try_from(bytes.len()).map_err(io::Error::other)?,
                    )
                    .await
                    .map_err(io::Error::other)?;
                if ready {
                    return self.publish_locked(output, bytes).await;
                }
                drop(publication);
                notified.await;
            }
        }
        let _publication = self.publication_gate.lock().await;
        self.publish_locked(output, bytes).await
    }

    async fn publish_locked(&self, output: OutputKey, mut bytes: Vec<u8>) -> io::Result<()> {
        self.runtime
            .validate_copy_sources(&self.preparation, 0)
            .await
            .map_err(io::Error::other)?;
        let publication = self
            .runtime
            .queue_scratch_publication_wait(&self.preparation, 0, self.member, output, &mut bytes)
            .await
            .map_err(io::Error::other)?;
        // A rejected queue still leaves the original bytes here; after a
        // known queue the original runtime cell owns them across cancellation.
        self.runtime
            .publish_scratch_output(
                &self.preparation,
                0,
                self.member,
                publication,
                std::time::Instant::now() + self.operation_timeout,
            )
            .await
            .map_err(io::Error::other)?;
        self.runtime
            .validate_copy_sources(&self.preparation, 0)
            .await
            .map_err(io::Error::other)
    }

    fn playlist(&self, end: bool) -> io::Result<String> {
        let mut text = fmp4::playlist_header(self.target_seconds);
        let end_bytes = if end { "#EXT-X-ENDLIST\n" } else { "" };
        text.try_reserve(self.entries.len() + end_bytes.len())
            .map_err(io::Error::other)?;
        text.push_str(&self.entries);
        text.push_str(end_bytes);
        Ok(text)
    }

    fn stage_playlist(&mut self, end: bool) -> io::Result<()> {
        let text = self.playlist(end)?;
        if text.len() > self.playlist_capacity {
            return Err(io::Error::other(
                "copy playlist grew beyond admitted capacity",
            ));
        }
        let mut last = self
            .produced_playlist
            .lock()
            .map_err(|_| io::Error::other("copy playlist cache poisoned"))?;
        if text.len() > last.capacity() {
            return Err(io::Error::other("copy playlist cache overflow"));
        }
        last.clear();
        last.push_str(&text);
        self.started = true;
        Ok(())
    }
}

impl PreparedCopySink {
    pub(in crate::adaptive_resources::runtime) fn bind(
        self,
        runtime: Arc<NodeResourceRuntime>,
        preparation: OwnedPreparation,
        member: Uuid,
    ) -> CopySink {
        debug_assert!(!member.is_nil());
        CopySink {
            runtime,
            preparation,
            member,
            max_segments: self.max_segments,
            max_payload_bytes: self.max_payload_bytes,
            reserved_memory_bytes: self.reserved_memory_bytes,
            gate_seconds: self.gate_seconds,
            target_seconds: self.target_seconds,
            operation_timeout: self.operation_timeout,
            entries: self.entries,
            produced_playlist: self.produced_playlist,
            served_playlist: self.served_playlist,
            publication_gate: self.publication_gate,
            progress: self.progress,
            playlist_capacity: self.playlist_capacity,
            published_seconds: 0.0,
            started: false,
            #[cfg(all(test, feature = "fixtures"))]
            fixture_panic_next_publication: false,
        }
    }
}

impl CopyPublicationSink for CopySink {
    async fn torn_down(&self) -> bool {
        // An I/O error is never physical close proof. The original graph owner
        // decides lifecycle teardown after joining this task and its pipe.
        false
    }

    async fn write_init(&mut self, init: &Init) -> io::Result<()> {
        self.publish(OutputKey::Init, init.bytes.clone()).await
    }

    async fn write_segment(&mut self, published: &Published) -> Result<(), PublishError> {
        let index = u32::try_from(published.index)
            .map_err(|_| PublishError::Contract("copy segment index overflow".into()))?;
        if index >= self.max_segments
            || !published.seconds.is_finite()
            || published.seconds <= 0.0
            || published.seconds > f64::from(self.target_seconds)
        {
            return Err(PublishError::Contract(
                "copy segment exceeds the fixed playlist plan".into(),
            ));
        }
        self.publish(
            OutputKey::Segment { index },
            published.segment.bytes.clone(),
        )
        .await
        .map_err(PublishError::Io)?;
        let name = published.name();
        let entry = fmp4::playlist_entry(published.seconds, &name);
        if entry.len() > MAX_PLAYLIST_ENTRY_BYTES
            || self
                .entries
                .len()
                .checked_add(entry.len())
                .is_none_or(|n| n > self.entries.capacity())
        {
            return Err(PublishError::Contract(
                "copy playlist entry exceeds its admitted capacity".into(),
            ));
        }
        self.entries.push_str(&entry);
        self.published_seconds += published.seconds;
        if self.started || self.published_seconds >= f64::from(self.gate_seconds) {
            self.stage_playlist(false).map_err(PublishError::Io)?;
        }
        Ok(())
    }

    async fn write_endlist(&mut self) -> io::Result<()> {
        if !self.entries.is_empty() {
            self.stage_playlist(true)?;
        }
        Ok(())
    }

    fn started(&self) -> bool {
        self.started
    }

    async fn verify_endlist(&self) -> io::Result<bool> {
        Ok(self
            .produced_playlist
            .lock()
            .map_err(|_| io::Error::other("copy playlist cache poisoned"))?
            .ends_with("#EXT-X-ENDLIST\n"))
    }
}
