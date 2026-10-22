//! Rendition adapter for the core finite-VOD publication owner.
use super::*;
use plurx_core::adaptive_resources::runtime::finite_vod::{FiniteOutput, PublicationTicket};
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
