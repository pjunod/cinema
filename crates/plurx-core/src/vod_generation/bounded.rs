//! Explicit parser and retained-fragment partition for a finite producer.
//! This is not a whole-process quota or a replacement for the outer R demand.
use super::*;
use crate::fmp4::{BoundedFragmentReader, Fmp4Error, FragmentReaderBounds};
#[derive(Clone, Copy, Debug)]
pub struct GenerationMemoryLimits {
    pub reader: FragmentReaderBounds,
    pub read_chunk_bytes: usize,
    pub retained_fragment_memory_bytes: usize,
    pub retained_fragments: usize,
}
impl GenerationMemoryLimits {
    pub fn validate(&self) -> Result<(), Fmp4Error> {
        self.reader.validate()?;
        if self.read_chunk_bytes == 0
            || self.read_chunk_bytes > self.reader.buffer_bytes
            || self.retained_fragments == 0
            || self.retained_fragment_memory_bytes == 0
        {
            return Err(Fmp4Error::Unsupported(
                "invalid finite generation memory partition".into(),
            ));
        }
        self.retained_fragments
            .checked_mul(std::mem::size_of::<Fragment>())
            .and_then(|n| n.checked_mul(2))
            .and_then(|n| {
                n.checked_add(
                    self.retained_fragments
                        .checked_mul(std::mem::size_of::<u64>())?,
                )
            })
            .filter(|n| *n <= self.retained_fragment_memory_bytes)
            .ok_or_else(|| {
                Fmp4Error::Unsupported("finite fragment inventory exceeds memory partition".into())
            })?;
        Ok(())
    }
}
pub(super) enum Reader {
    Legacy(FragmentReader),
    Bounded(BoundedFragmentReader),
}
impl Reader {
    pub(super) fn new(limits: Option<GenerationMemoryLimits>) -> Result<Self, Fmp4Error> {
        match limits {
            Some(limits) => {
                limits.validate()?;
                Ok(Self::Bounded(BoundedFragmentReader::new(limits.reader)?))
            }
            None => Ok(Self::Legacy(FragmentReader::new())),
        }
    }
    pub(super) fn read_capacity(&self, buffer: usize) -> usize {
        match self {
            Self::Legacy(_) => buffer,
            Self::Bounded(r) => buffer.min(r.remaining_capacity()),
        }
    }
    pub(super) fn push(&mut self, data: &[u8]) -> Result<(), Fmp4Error> {
        match self {
            Self::Legacy(r) => {
                r.push(data);
                Ok(())
            }
            Self::Bounded(r) => r.try_push(data),
        }
    }
    pub(super) fn next_unit(&mut self) -> Result<Option<Unit>, Fmp4Error> {
        match self {
            Self::Legacy(r) => r.next_unit(),
            Self::Bounded(r) => r.next_unit(),
        }
    }
    pub(super) fn buffered(&self) -> usize {
        match self {
            Self::Legacy(r) => r.buffered(),
            Self::Bounded(r) => r.buffered(),
        }
    }
    pub(super) fn saw_trailer(&self) -> bool {
        match self {
            Self::Legacy(r) => r.saw_trailer(),
            Self::Bounded(r) => r.saw_trailer(),
        }
    }
}
fn refusal(error: impl std::fmt::Display) -> Outcome {
    Outcome::Failed(Failure::Stream(format!(
        "finite generation memory: {error}"
    )))
}
impl<S: Sink> GenerationRun<'_, S> {
    pub(super) fn check_fragment_retention(&self, incoming: &Fragment) -> Result<(), Outcome> {
        let Some(limits) = self.memory_limits else {
            return Ok(());
        };
        let mut count = self
            .landing
            .len()
            .checked_add(self.transferring_fragments)
            .ok_or_else(|| refusal("transferring fragment count overflow"))?;
        let mut bytes = self
            .landing
            .capacity()
            .checked_mul(std::mem::size_of::<Fragment>())
            .and_then(|n| {
                n.checked_add(
                    self.observed
                        .capacity()
                        .checked_mul(std::mem::size_of::<u64>())?,
                )
            })
            .ok_or_else(|| refusal("inventory capacity overflow"))?;
        bytes = bytes
            .checked_add(self.transferring_fragment_memory_bytes)
            .ok_or_else(|| refusal("transferring fragment memory overflow"))?;
        for fragment in &self.landing {
            let heap = fragment
                .resident_memory_bytes()
                .map_err(refusal)?
                .checked_sub(std::mem::size_of::<Fragment>())
                .ok_or_else(|| refusal("fragment size mismatch"))?;
            bytes = bytes
                .checked_add(heap)
                .ok_or_else(|| refusal("fragment memory overflow"))?;
        }
        if let Some(segmenter) = &self.segmenter {
            count = count
                .checked_add(segmenter.pending_fragments())
                .ok_or_else(|| refusal("fragment count overflow"))?;
            bytes = bytes
                .checked_add(segmenter.resident_memory_bytes().map_err(refusal)?)
                .ok_or_else(|| refusal("segment memory overflow"))?;
        }
        bytes = bytes
            .checked_add(incoming.resident_memory_bytes().map_err(refusal)?)
            .ok_or_else(|| refusal("incoming fragment memory overflow"))?;
        if count
            .checked_add(1)
            .is_none_or(|n| n > limits.retained_fragments)
            || bytes > limits.retained_fragment_memory_bytes
        {
            return Err(refusal("retained fragment partition exhausted"));
        }
        Ok(())
    }
}
