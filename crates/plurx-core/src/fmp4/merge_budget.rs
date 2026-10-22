//! Allocation preflight for the finite, untransformed AVC merge path.
//! Original fragment storage belongs to the caller's retained partition.
use super::*;
use std::mem::size_of;

/// Separately reserved merge metadata and serialized output ceilings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MergeMemoryLimits {
    pub workspace_bytes: usize,
    pub output_bytes: usize,
    pub max_tracks: usize,
    pub max_samples: usize,
}

impl MergeMemoryLimits {
    pub fn validate(self) -> Result<(), Fmp4Error> {
        if self.workspace_bytes == 0
            || self.output_bytes == 0
            || self.max_tracks == 0
            || self.max_samples == 0
            || self.max_tracks > u32::MAX as usize
            || self.max_samples > u32::MAX as usize
        {
            return Err(Fmp4Error::Unsupported(
                "invalid AVC merge memory limits".into(),
            ));
        }
        Ok(())
    }
}

pub(super) struct Preflight {
    pub track_slots: usize,
    pub plan_count: usize,
    pub moof_bytes: usize,
    pub output_bytes: usize,
}

fn exceeded(what: &str) -> Fmp4Error {
    Fmp4Error::Unsupported(format!("bounded AVC merge {what} exceeds its partition"))
}
fn overflow() -> Fmp4Error {
    Fmp4Error::Unsupported("bounded AVC merge size overflows".into())
}
fn add(a: usize, b: usize) -> Result<usize, Fmp4Error> {
    a.checked_add(b).ok_or_else(overflow)
}
fn mul(a: usize, b: usize) -> Result<usize, Fmp4Error> {
    a.checked_mul(b).ok_or_else(overflow)
}

/// Scan the actual source runs without allocating a parallel index. Only the
/// first track fragment for each ID is meaningful to the legacy merge, so
/// duplicate IDs are refused rather than counted differently from its output.
pub(super) fn preflight(
    fragments: &[Fragment],
    init: &Init,
    retain_parameter_sets: bool,
    limits: MergeMemoryLimits,
) -> Result<Preflight, Fmp4Error> {
    limits.validate()?;
    if retain_parameter_sets
        || init
            .video()
            .is_none_or(|video| video.codec != Some(VideoCodec::H264))
        || init.tracks.iter().any(|track| {
            track.kind == TrackKind::Video
                && (track.codec != Some(VideoCodec::H264) || track.dolby_vision_config)
        })
    {
        return Err(Fmp4Error::Unsupported(
            "bounded merge supports ordinary AVC without HEVC retention".into(),
        ));
    }
    if fragments.is_empty() {
        return malformed("nothing to merge");
    }
    if init.tracks.len() > limits.max_tracks {
        return Err(exceeded("track count"));
    }
    for (index, track) in init.tracks.iter().enumerate() {
        if init.tracks[..index]
            .iter()
            .any(|prior| prior.id == track.id)
        {
            return malformed("duplicate init track ID in bounded AVC merge");
        }
    }
    let mut track_slots = 0;
    let mut plan_count = 0;
    let mut samples = 0usize;
    let mut payload = 0usize;
    let mut moof_bytes = 24usize; // moof header plus mfhd
    for track in &init.tracks {
        let mut present = false;
        let mut track_samples = 0usize;
        let mut composition_offsets = false;
        for fragment in fragments {
            for (index, tf) in fragment.tracks.iter().enumerate() {
                if fragment.tracks[..index]
                    .iter()
                    .any(|prior| prior.track_id == tf.track_id)
                {
                    return malformed("duplicate fragment track ID in bounded AVC merge");
                }
                if init.track(tf.track_id).is_none() {
                    return malformed("fragment names an unknown track in bounded AVC merge");
                }
            }
            let Some(tf) = fragment.track(track.id) else {
                continue;
            };
            present = true;
            let mut duration = 0u64;
            for run in &tf.runs {
                let mut start = run.data_offset;
                for sample in &run.samples {
                    let end = add(start, sample.size as usize)?;
                    if end > fragment.bytes.len() {
                        return malformed("a sample points past the end of its fragment");
                    }
                    start = end;
                    samples = add(samples, 1)?;
                    track_samples = add(track_samples, 1)?;
                    payload = add(payload, sample.size as usize)?;
                    duration = duration
                        .checked_add(u64::from(sample.duration))
                        .ok_or_else(overflow)?;
                    composition_offsets |= sample.cto != 0;
                    if samples > limits.max_samples {
                        return Err(exceeded("sample count"));
                    }
                }
            }
            tf.base_decode_time
                .checked_add(duration)
                .ok_or_else(overflow)?;
        }
        if present {
            track_slots = add(track_slots, 1)?;
            if track_slots > limits.max_tracks {
                return Err(exceeded("track count"));
            }
        }
        if track_samples > 0 {
            plan_count = add(plan_count, 1)?;
            let per = if composition_offsets { 16 } else { 12 };
            let traf = add(64, mul(track_samples, per)?)?;
            if traf > u32::MAX as usize {
                return Err(exceeded("traf size"));
            }
            moof_bytes = add(moof_bytes, traf)?;
        }
    }
    if plan_count == 0 {
        return malformed("no track survived the merge");
    }
    if moof_bytes > u32::MAX as usize {
        return Err(exceeded("moof size"));
    }
    let mdat_bytes = add(payload, 8)?;
    if mdat_bytes > u32::MAX as usize {
        return Err(exceeded("mdat size"));
    }
    let referenced = add(moof_bytes, mdat_bytes)?;
    if referenced > 0x7fff_ffff {
        return Err(exceeded("sidx reference size"));
    }
    let output_bytes = add(add(STYP.len(), mul(plan_count, SIDX_BYTES)?)?, referenced)?;
    if output_bytes > limits.output_bytes {
        return Err(exceeded("output bytes"));
    }
    let workspace = add(
        add(
            mul(track_slots, size_of::<u32>())?,
            mul(track_slots, size_of::<Plan>())?,
        )?,
        add(
            mul(samples, add(size_of::<Sample>(), size_of::<Slice>())?)?,
            add(mul(plan_count, size_of::<usize>())?, moof_bytes)?,
        )?,
    )?;
    if workspace > limits.workspace_bytes {
        return Err(exceeded("workspace bytes"));
    }
    Ok(Preflight {
        track_slots,
        plan_count,
        moof_bytes,
        output_bytes,
    })
}

pub(super) fn reserve<T>(count: usize) -> Result<Vec<T>, Fmp4Error> {
    let mut out = Vec::new();
    out.try_reserve_exact(count)
        .map_err(|_| Fmp4Error::Unsupported("bounded AVC merge reservation failed".into()))?;
    if out.capacity() > count {
        return Err(exceeded("actual allocation capacity"));
    }
    Ok(out)
}

pub(super) fn samples_for_track(fragments: &[Fragment], id: u32) -> Result<usize, Fmp4Error> {
    let mut count = 0usize;
    for fragment in fragments {
        if let Some(track) = fragment.track(id) {
            for run in &track.runs {
                count = add(count, run.samples.len())?;
            }
        }
    }
    Ok(count)
}

fn put(out: &mut Vec<u8>, bytes: &[u8], limit: usize) -> Result<(), Fmp4Error> {
    if add(out.len(), bytes.len())? > limit {
        return Err(exceeded("serialized moof size"));
    }
    out.extend_from_slice(bytes);
    Ok(())
}

/// Same box layout as `build_moof`, with one verified reservation and checked
/// writes so the builder cannot silently grow its Vec.
pub(super) fn build_moof_bounded(
    plans: &[Plan],
    sequence: u32,
    preflight: &Preflight,
    offset_slots: &mut Vec<usize>,
) -> Result<Vec<u8>, Fmp4Error> {
    let mut moof = reserve::<u8>(preflight.moof_bytes)?;
    let limit = preflight.moof_bytes;
    put(&mut moof, &[0, 0, 0, 0], limit)?;
    put(&mut moof, b"moof", limit)?;
    put(&mut moof, &16u32.to_be_bytes(), limit)?;
    put(&mut moof, b"mfhd", limit)?;
    put(&mut moof, &0u32.to_be_bytes(), limit)?;
    put(&mut moof, &sequence.to_be_bytes(), limit)?;
    for plan in plans {
        let traf_start = moof.len();
        put(&mut moof, &[0, 0, 0, 0], limit)?;
        put(&mut moof, b"traf", limit)?;
        put(&mut moof, &16u32.to_be_bytes(), limit)?;
        put(&mut moof, b"tfhd", limit)?;
        put(&mut moof, &0x0002_0000u32.to_be_bytes(), limit)?;
        put(&mut moof, &plan.track_id.to_be_bytes(), limit)?;
        put(&mut moof, &20u32.to_be_bytes(), limit)?;
        put(&mut moof, b"tfdt", limit)?;
        put(&mut moof, &0x0100_0000u32.to_be_bytes(), limit)?;
        put(&mut moof, &plan.base_decode_time.to_be_bytes(), limit)?;
        let flags = trun_flags(&plan.samples);
        let version = trun_version(&plan.samples);
        let has_cto = flags & 0x0000_0800 != 0;
        let per = if has_cto { 16 } else { 12 };
        let size = add(20, mul(plan.samples.len(), per)?)?;
        put(&mut moof, &(size as u32).to_be_bytes(), limit)?;
        put(&mut moof, b"trun", limit)?;
        put(
            &mut moof,
            &(((version as u32) << 24) | flags).to_be_bytes(),
            limit,
        )?;
        put(&mut moof, &(plan.samples.len() as u32).to_be_bytes(), limit)?;
        if offset_slots.len() >= preflight.plan_count {
            return Err(exceeded("offset slots"));
        }
        offset_slots.push(moof.len());
        put(&mut moof, &0u32.to_be_bytes(), limit)?;
        for sample in &plan.samples {
            put(&mut moof, &sample.duration.to_be_bytes(), limit)?;
            put(&mut moof, &sample.size.to_be_bytes(), limit)?;
            put(&mut moof, &sample.flags.to_be_bytes(), limit)?;
            if has_cto {
                let cto = sample.cto.clamp(i32::MIN as i64, i32::MAX as i64) as i32;
                put(&mut moof, &cto.to_be_bytes(), limit)?;
            }
        }
        let traf_len = (moof.len() - traf_start) as u32;
        moof[traf_start..traf_start + 4].copy_from_slice(&traf_len.to_be_bytes());
    }
    if moof.len() != limit || offset_slots.len() != preflight.plan_count {
        return Err(exceeded("moof shape"));
    }
    let moof_len = moof.len() as u32;
    moof[..4].copy_from_slice(&moof_len.to_be_bytes());
    Ok(moof)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn avc_source() -> (Init, Vec<Fragment>) {
        let init = Init {
            bytes: Vec::new(),
            tracks: vec![Track {
                id: 1,
                kind: TrackKind::Video,
                timescale: 90_000,
                has_edit_list: false,
                codec: Some(VideoCodec::H264),
                dolby_vision_config: false,
                nal_length_size: 4,
                default_sample_duration: 3_000,
                default_sample_size: 4,
                default_sample_flags: 0,
            }],
        };
        let fragments = vec![Fragment {
            bytes: vec![1, 2, 3, 4, 5, 6, 7, 8],
            mdat_payload: 0..8,
            tracks: vec![TrackFragment {
                track_id: 1,
                base_decode_time: 0,
                runs: vec![Run {
                    version: 1,
                    composition_offsets_present: true,
                    data_offset: 0,
                    data_offset_at: None,
                    samples: vec![
                        Sample {
                            duration: 3_000,
                            size: 4,
                            size_at: None,
                            flags: 0,
                            cto: -300,
                        },
                        Sample {
                            duration: 3_000,
                            size: 4,
                            size_at: None,
                            flags: 0,
                            cto: 0,
                        },
                    ],
                }],
            }],
        }];
        (init, fragments)
    }

    fn limits() -> MergeMemoryLimits {
        MergeMemoryLimits {
            workspace_bytes: 4096,
            output_bytes: 4096,
            max_tracks: 1,
            max_samples: 2,
        }
    }

    #[test]
    fn bounded_avc_merge_matches_legacy_bytes_and_ticks() {
        let (init, fragments) = avc_source();
        let legacy = merge_retaining(&fragments, &init, 7, false).unwrap();
        let mut budget = limits();
        budget.output_bytes = legacy.bytes.len();
        let bounded = merge_retaining_bounded(&fragments, &init, 7, false, budget).unwrap();
        assert_eq!(bounded, legacy);
    }

    #[test]
    fn insufficient_output_partition_refuses_before_copy() {
        let (init, fragments) = avc_source();
        let expected = merge_retaining(&fragments, &init, 7, false).unwrap();
        let mut budget = limits();
        budget.output_bytes = expected.bytes.len() - 1;
        assert!(matches!(
            merge_retaining_bounded(&fragments, &init, 7, false, budget),
            Err(Fmp4Error::Unsupported(_))
        ));
    }
}
