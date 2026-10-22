//! Checked Rust-owned capacities, excluding allocator, Tokio and OS baselines.
//! These observations do not authorize an allocation or prove physical closure.
use super::*;
use std::mem::size_of;
fn overflow() -> Fmp4Error {
    Fmp4Error::Unsupported("fMP4 resident memory accounting overflow".into())
}
fn allocation<T>(capacity: usize) -> Result<usize, Fmp4Error> {
    capacity.checked_mul(size_of::<T>()).ok_or_else(overflow)
}
impl Init {
    pub fn resident_memory_bytes(&self) -> Result<usize, Fmp4Error> {
        size_of::<Self>()
            .checked_add(self.bytes.capacity())
            .and_then(|n| n.checked_add(self.tracks.capacity().checked_mul(size_of::<Track>())?))
            .ok_or_else(overflow)
    }
}
impl Fragment {
    pub fn resident_memory_bytes(&self) -> Result<usize, Fmp4Error> {
        let mut bytes = size_of::<Self>()
            .checked_add(self.bytes.capacity())
            .and_then(|n| {
                n.checked_add(
                    self.tracks
                        .capacity()
                        .checked_mul(size_of::<TrackFragment>())?,
                )
            })
            .ok_or_else(overflow)?;
        for track in &self.tracks {
            bytes = bytes
                .checked_add(allocation::<Run>(track.runs.capacity())?)
                .ok_or_else(overflow)?;
            for run in &track.runs {
                bytes = bytes
                    .checked_add(allocation::<Sample>(run.samples.capacity())?)
                    .ok_or_else(overflow)?;
            }
        }
        Ok(bytes)
    }
}
impl Segmenter {
    pub fn pending_fragments(&self) -> usize {
        self.pending.len()
    }
    /// Reserve the entire finite pending inventory before its first push.
    pub fn reserve_pending_fragments(&mut self, count: usize) -> Result<(), Fmp4Error> {
        if count == 0 || !self.pending.is_empty() {
            return malformed("finite pending inventory is not empty");
        }
        self.pending
            .try_reserve_exact(count)
            .map_err(|_| overflow())?;
        if self.pending.capacity() > count {
            return malformed("finite pending inventory capacity exceeded its bound");
        }
        Ok(())
    }

    pub fn resident_memory_bytes(&self) -> Result<usize, Fmp4Error> {
        let mut bytes = size_of::<Self>()
            .checked_add(
                self.init
                    .resident_memory_bytes()?
                    .checked_sub(size_of::<Init>())
                    .ok_or_else(overflow)?,
            )
            .and_then(|n| {
                n.checked_add(self.pending.capacity().checked_mul(size_of::<Fragment>())?)
            })
            .ok_or_else(overflow)?;
        if let Some(boundaries) = &self.boundaries {
            bytes = bytes
                .checked_add(allocation::<u64>(boundaries.starts.capacity())?)
                .ok_or_else(overflow)?;
        }
        for fragment in &self.pending {
            bytes = bytes
                .checked_add(
                    fragment
                        .resident_memory_bytes()?
                        .checked_sub(size_of::<Fragment>())
                        .ok_or_else(overflow)?,
                )
                .ok_or_else(overflow)?;
        }
        Ok(bytes)
    }
}
