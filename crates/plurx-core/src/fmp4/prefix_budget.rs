//! Exact, allocation-free preflight for the encoded AAC prefix moved at a cut.
use super::*;
use std::mem::size_of;

#[derive(Clone, Copy)]
pub(super) struct PrefixShape {
    pub bytes: usize,
    pub tracks: usize,
    pub runs: usize,
    pub samples: usize,
}
impl PrefixShape {
    pub fn present(self) -> bool {
        self.tracks != 0
    }
}

fn overflow() -> Fmp4Error {
    Fmp4Error::Unsupported("bounded encoded audio prefix size overflows".into())
}

/// The same sample-start predicate as `take_encoded_audio_before`, without
/// copying bytes or mutating the original runs.
pub(super) fn selected_runs(
    track: &TrackFragment,
    info: &Track,
    boundary: u64,
    video_timescale: u32,
) -> Result<usize, Fmp4Error> {
    let cutoff = u128::from(boundary) * u128::from(info.timescale);
    let mut time = track.base_decode_time;
    let mut selected = 0usize;
    for run in &track.runs {
        let mut count = 0usize;
        for sample in &run.samples {
            if u128::from(time) * u128::from(video_timescale) >= cutoff {
                break;
            }
            time = time
                .checked_add(u64::from(sample.duration))
                .ok_or_else(overflow)?;
            count += 1;
        }
        if count == 0 {
            break;
        }
        selected = selected.checked_add(1).ok_or_else(overflow)?;
        if count < run.samples.len() {
            break;
        }
    }
    Ok(selected)
}

pub(super) fn preflight(
    fragment: &Fragment,
    init: &Init,
    boundary: u64,
    video_timescale: u32,
    limit: usize,
) -> Result<PrefixShape, Fmp4Error> {
    let mut shape = PrefixShape {
        bytes: 0,
        tracks: 0,
        runs: 0,
        samples: 0,
    };
    for track in &fragment.tracks {
        let Some(info) = init
            .tracks
            .iter()
            .find(|info| info.id == track.track_id && info.kind == TrackKind::Audio)
        else {
            continue;
        };
        let cutoff = u128::from(boundary) * u128::from(info.timescale);
        let mut time = track.base_decode_time;
        let mut track_has_prefix = false;
        for run in &track.runs {
            let mut count = 0usize;
            let mut length = 0usize;
            for sample in &run.samples {
                if u128::from(time) * u128::from(video_timescale) >= cutoff {
                    break;
                }
                time = time
                    .checked_add(u64::from(sample.duration))
                    .ok_or_else(overflow)?;
                length = length
                    .checked_add(sample.size as usize)
                    .ok_or_else(overflow)?;
                count += 1;
            }
            if count == 0 {
                break;
            }
            let end = run.data_offset.checked_add(length).ok_or_else(overflow)?;
            if fragment.bytes.get(run.data_offset..end).is_none() {
                return malformed("encoded audio leaves its payload");
            }
            track_has_prefix = true;
            shape.runs = shape.runs.checked_add(1).ok_or_else(overflow)?;
            shape.samples = shape.samples.checked_add(count).ok_or_else(overflow)?;
            shape.bytes = shape.bytes.checked_add(length).ok_or_else(overflow)?;
            if count < run.samples.len() {
                break;
            }
        }
        if track_has_prefix {
            shape.tracks = shape.tracks.checked_add(1).ok_or_else(overflow)?;
        }
    }
    let needed = shape
        .bytes
        .checked_add(
            shape
                .tracks
                .checked_mul(size_of::<TrackFragment>())
                .ok_or_else(overflow)?,
        )
        .and_then(|n| n.checked_add(shape.runs.checked_mul(size_of::<Run>())?))
        .and_then(|n| n.checked_add(shape.samples.checked_mul(size_of::<Sample>())?))
        .ok_or_else(overflow)?;
    if needed > limit {
        return Err(Fmp4Error::Unsupported(
            "encoded audio prefix exceeds its memory partition".into(),
        ));
    }
    Ok(shape)
}
