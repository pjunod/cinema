//! Rendition adapter for the core finite-VOD publication owner.
use super::*;
use plurx_core::adaptive_resources::runtime::finite_vod::{
    FiniteFile, FiniteOutput, PublicationTicket,
};
use std::time::{Duration, Instant};

pub(super) struct FinitePublication {
    output: FiniteOutput,
    pending: tokio::sync::Mutex<Option<PublicationTicket>>,
}

impl std::fmt::Debug for FinitePublication {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FinitePublication").finish_non_exhaustive()
    }
}

impl FinitePublication {
    pub(super) fn new(output: FiniteOutput) -> Self {
        Self {
            output,
            pending: tokio::sync::Mutex::new(None),
        }
    }

    pub(super) async fn join(&self) -> io::Result<()> {
        let mut pending = self.pending.lock().await;
        self.finish_pending(&mut pending).await
    }

    pub(super) async fn sync(&self, manifest: &Manifest) -> io::Result<()> {
        let mut pending = self.pending.lock().await;
        self.finish_pending(&mut pending).await?;
        let mut files = Vec::with_capacity(16);
        files.extend([FiniteFile::Init, FiniteFile::IdentityMetadata]);
        if self.output.has_encoded_process_metadata() {
            files.push(FiniteFile::EncodedProcessMetadata);
        }
        for index in 0..manifest.len() {
            let index = u32::try_from(index).map_err(io::Error::other)?;
            if manifest.state(index).is_some_and(|s| s.is_materialized()) {
                files.push(FiniteFile::Segment(index));
                if files.len() == 16 {
                    *pending = Some(
                        self.output
                            .queue_sync(&files)
                            .map_err(|e| io::Error::other(e.to_string()))?,
                    );
                    self.finish_pending(&mut pending).await?;
                    files.clear();
                }
            }
        }
        if !files.is_empty() {
            *pending = Some(
                self.output
                    .queue_sync(&files)
                    .map_err(|e| io::Error::other(e.to_string()))?,
            );
            self.finish_pending(&mut pending).await?;
        }
        Ok(())
    }

    pub(super) async fn publish_metadata(&self, name: &str, mut bytes: Vec<u8>) -> io::Result<()> {
        let mut pending = self.pending.lock().await;
        self.finish_pending(&mut pending).await?;
        let ticket = match name {
            "identity.json" => self.output.queue_identity_metadata(&mut bytes),
            "encoded-process" => self.output.queue_encoded_process_metadata(&mut bytes),
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "unknown finite metadata name",
                ))
            }
        }
        .map_err(|e| io::Error::other(e.to_string()))?;
        *pending = Some(ticket);
        self.finish_pending(&mut pending).await
    }

    pub(super) async fn publish(&self, name: &str, mut bytes: Vec<u8>) -> io::Result<()> {
        let mut pending = self.pending.lock().await;
        self.finish_pending(&mut pending).await?;
        let ticket = if name == INIT_NAME {
            self.output.queue_init(&mut bytes)
        } else {
            let index = parse_segment_name(name)
                .and_then(|index| u32::try_from(index).ok())
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "invalid finite segment name")
                })?;
            if segment_name(u64::from(index)) != name {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "noncanonical finite segment name",
                ));
            }
            self.output.queue_segment(index, &mut bytes)
        }
        .map_err(|error| io::Error::other(error.to_string()))?;
        // Queueing is synchronous; custody retains the accepted ticket before
        // the first await, so cancellation only releases this borrow.
        *pending = Some(ticket);
        self.finish_pending(&mut pending).await
    }

    async fn finish_pending(&self, pending: &mut Option<PublicationTicket>) -> io::Result<()> {
        let Some(ticket) = pending.as_ref() else {
            return Ok(());
        };
        self.output
            .finish(ticket, Instant::now() + Duration::from_secs(30))
            .await
            .map_err(|error| io::Error::other(error.to_string()))?;
        *pending = None;
        Ok(())
    }
}

impl RenditionDir {
    #[cfg(target_os = "linux")]
    pub(crate) fn finite_producer(
        &self,
    ) -> Option<std::sync::Arc<crate::vodserve::finite_software::FiniteSoftwareProducer>> {
        self.writes
            .finite_producer
            .as_ref()
            .map(std::sync::Arc::clone)
    }
    #[cfg(target_os = "linux")]
    pub(crate) fn from_finite_producer(
        owner: plurx_core::adaptive_resources::runtime::finite_vod::SoftwareVodOwner,
    ) -> Self {
        let output = owner.output();
        let producer = std::sync::Arc::new(
            crate::vodserve::finite_software::FiniteSoftwareProducer::new(owner),
        );
        Self {
            dir: output.path().to_owned(),
            writes: std::sync::Arc::new(PendingWrites {
                finite: Some(FinitePublication::new(output)),
                finite_producer: Some(producer),
                ..PendingWrites::default()
            }),
        }
    }
    pub(crate) fn is_finite(&self) -> bool {
        self.writes.finite.is_some()
    }

    /// Return the finite output selector without exposing or duplicating its
    /// held native directory handle.
    pub(crate) fn finite_output(&self) -> Option<FiniteOutput> {
        self.writes
            .finite
            .as_ref()
            .map(|finite| finite.output.clone())
    }
    pub(crate) async fn write_finite_metadata(&self, name: &str, bytes: Vec<u8>) -> io::Result<()> {
        self.writes
            .finite
            .as_ref()
            .ok_or_else(|| io::Error::other("finite metadata owner absent"))?
            .publish_metadata(name, bytes)
            .await
    }
    pub(crate) fn from_finite(output: FiniteOutput) -> RenditionDir {
        RenditionDir {
            dir: output.path().to_owned(),
            writes: std::sync::Arc::new(PendingWrites {
                finite: Some(FinitePublication::new(output)),
                ..PendingWrites::default()
            }),
        }
    }
}
