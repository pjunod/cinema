//! Shared bounded-finish walk: one grouping decision feeds preflight and copy.
use super::*;
use std::mem::size_of;

pub(super) struct Group<'a> {
    pub chunk: usize,
    pub fragment: &'a Fragment,
    pub track_id: u32,
    pub run: &'a Run,
    pub samples: &'a [Sample],
    pub decode_time: u64,
    pub data_offset: usize,
}

fn overflow() -> Fmp4Error {
    Fmp4Error::Unsupported("bounded final split size overflows".into())
}
fn add(a: usize, b: usize) -> Result<usize, Fmp4Error> {
    a.checked_add(b).ok_or_else(overflow)
}
fn mul(a: usize, b: usize) -> Result<usize, Fmp4Error> {
    a.checked_mul(b).ok_or_else(overflow)
}

/// Mirrors `end_of_stream_chunks` without any allocation. The bounded
/// preflight and builder both call this function, so group boundaries agree.
pub(super) fn walk_groups<'a>(
    segmenter: &'a Segmenter,
    mut visit: impl FnMut(Group<'a>) -> Result<(), Fmp4Error>,
) -> Result<(), Fmp4Error> {
    let mut video_ticks = 0u64;
    for fragment in &segmenter.pending {
        video_ticks = video_ticks
            .checked_add(fragment.video_duration(&segmenter.init))
            .ok_or_else(overflow)?;
    }
    for track in &segmenter.init.tracks {
        let ceiling = scale_ticks(
            segmenter.policy.max_ticks,
            segmenter.video_timescale,
            track.timescale.max(1),
        )
        .max(1);
        let video_limit = scale_ticks(
            video_ticks,
            segmenter.video_timescale,
            track.timescale.max(1),
        );
        let first_limit = if segmenter.boundaries.is_some() {
            video_limit.max(1)
        } else {
            ceiling.max(video_limit)
        };
        let allow_boundary_sample = video_ticks > 0;
        let mut chunk_index = 0usize;
        let mut chunk_ticks = 0u64;
        for fragment in &segmenter.pending {
            let Some(track_fragment) = fragment.track(track.id) else {
                continue;
            };
            let mut decode_time = track_fragment.base_decode_time;
            for run in &track_fragment.runs {
                let mut data_offset = run.data_offset;
                let mut group_start = 0usize;
                while group_start < run.samples.len() {
                    let sample = run.samples[group_start];
                    let limit = if chunk_index == 0 {
                        first_limit
                    } else {
                        ceiling
                    };
                    if sample.duration as u64 > ceiling {
                        return Err(Fmp4Error::Unsupported(
                            "sample exceeds bounded final duration ceiling".into(),
                        ));
                    }
                    let crosses_limit = chunk_ticks > 0
                        && chunk_ticks.saturating_add(sample.duration as u64) > limit;
                    let starts_after_video_boundary =
                        chunk_index == 0 && allow_boundary_sample && chunk_ticks >= limit;
                    if starts_after_video_boundary
                        || (crosses_limit && !(chunk_index == 0 && allow_boundary_sample))
                    {
                        chunk_index = add(chunk_index, 1)?;
                        chunk_ticks = 0;
                        continue;
                    }
                    let selected_chunk = chunk_index;
                    let selected_decode_time = decode_time;
                    let selected_data_offset = data_offset;
                    let mut group_end = group_start;
                    while group_end < run.samples.len() {
                        let candidate = run.samples[group_end];
                        if candidate.duration as u64 > ceiling {
                            return Err(Fmp4Error::Unsupported(
                                "sample exceeds bounded final duration ceiling".into(),
                            ));
                        }
                        let limit = if chunk_index == 0 {
                            first_limit
                        } else {
                            ceiling
                        };
                        let crosses_limit = chunk_ticks > 0
                            && chunk_ticks.saturating_add(candidate.duration as u64) > limit;
                        let starts_after_video_boundary =
                            chunk_index == 0 && allow_boundary_sample && chunk_ticks >= limit;
                        if starts_after_video_boundary
                            || (crosses_limit && !(chunk_index == 0 && allow_boundary_sample))
                        {
                            break;
                        }
                        chunk_ticks = chunk_ticks
                            .checked_add(candidate.duration as u64)
                            .ok_or_else(overflow)?;
                        decode_time = decode_time
                            .checked_add(candidate.duration as u64)
                            .ok_or_else(overflow)?;
                        data_offset = add(data_offset, candidate.size as usize)?;
                        group_end += 1;
                    }
                    visit(Group {
                        chunk: selected_chunk,
                        fragment,
                        track_id: track.id,
                        run,
                        samples: &run.samples[group_start..group_end],
                        decode_time: selected_decode_time,
                        data_offset: selected_data_offset,
                    })?;
                    group_start = group_end;
                }
            }
        }
    }
    Ok(())
}

pub(super) struct SplitShape {
    pub chunk_slots: usize,
}

pub(super) fn preflight(segmenter: &Segmenter, limit: usize) -> Result<SplitShape, Fmp4Error> {
    let mut chunk_slots = 0usize;
    let mut group_count = 0usize;
    let mut bytes = 0usize;
    walk_groups(segmenter, |group| {
        chunk_slots = chunk_slots.max(add(group.chunk, 1)?);
        group_count = add(group_count, 1)?;
        let group_bytes = add(
            add(size_of::<Fragment>(), group.fragment.bytes.len())?,
            add(
                add(size_of::<TrackFragment>(), size_of::<Run>())?,
                mul(group.samples.len(), size_of::<Sample>())?,
            )?,
        )?;
        bytes = add(bytes, group_bytes)?;
        Ok(())
    })?;
    if group_count == 0 {
        return malformed("no track survived the end-of-stream split");
    }
    bytes = add(bytes, mul(chunk_slots, size_of::<Vec<Fragment>>())?)?;
    if bytes > limit {
        return Err(Fmp4Error::Unsupported(
            "bounded final split exceeds its memory partition".into(),
        ));
    }
    Ok(SplitShape { chunk_slots })
}

pub(super) fn chunks(
    segmenter: &Segmenter,
    shape: &SplitShape,
) -> Result<Vec<Vec<Fragment>>, Fmp4Error> {
    let mut chunks: Vec<Vec<Fragment>> = merge_budget::reserve(shape.chunk_slots)?;
    for chunk_index in 0..shape.chunk_slots {
        let mut count = 0usize;
        walk_groups(segmenter, |group| {
            if group.chunk == chunk_index {
                count = add(count, 1)?;
            }
            Ok(())
        })?;
        chunks.push(merge_budget::reserve(count)?);
    }
    walk_groups(segmenter, |group| {
        let mut bytes = merge_budget::reserve::<u8>(group.fragment.bytes.len())?;
        bytes.extend_from_slice(&group.fragment.bytes);
        let mut samples = merge_budget::reserve::<Sample>(group.samples.len())?;
        samples.extend_from_slice(group.samples);
        let mut runs = merge_budget::reserve::<Run>(1)?;
        runs.push(Run {
            version: group.run.version,
            composition_offsets_present: group.run.composition_offsets_present,
            data_offset_at: None,
            data_offset: group.data_offset,
            samples,
        });
        let mut tracks = merge_budget::reserve::<TrackFragment>(1)?;
        tracks.push(TrackFragment {
            track_id: group.track_id,
            base_decode_time: group.decode_time,
            runs,
        });
        let chunk = &mut chunks[group.chunk];
        if chunk.len() >= chunk.capacity() {
            return malformed("bounded final chunk exceeds reserved group count");
        }
        chunk.push(Fragment {
            bytes,
            mdat_payload: group.fragment.mdat_payload.clone(),
            tracks,
        });
        Ok(())
    })?;
    chunks.retain(|chunk| !chunk.is_empty());
    if chunks.is_empty() {
        return malformed("no track survived the end-of-stream split");
    }
    Ok(chunks)
}
