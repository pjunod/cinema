//! Complete finite AVC segmenter route. Retained source fragments are charged
//! separately by the caller; these limits cover prefix, split and output peaks.
use super::*;
use std::mem::size_of;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FiniteSegmenterLimits {
    pub merge: MergeMemoryLimits,
    pub prefix_memory_bytes: usize,
    pub split_memory_bytes: usize,
    pub published_batch_bytes: usize,
    pub retained_fragments: usize,
}

impl FiniteSegmenterLimits {
    pub fn validate(self) -> Result<(), Fmp4Error> {
        self.merge.validate()?;
        if self.prefix_memory_bytes == 0
            || self.split_memory_bytes == 0
            || self.published_batch_bytes == 0
            || self.retained_fragments == 0
        {
            return Err(Fmp4Error::Unsupported(
                "invalid finite AVC segmenter limits".into(),
            ));
        }
        Ok(())
    }
}

impl Segmenter {
    /// Construct the encoded AVC path before any fragment is admitted. The
    /// pending Vec reservation is fixed for this segmenter's whole lifetime.
    pub fn following_encoded_bounded(
        init: Init,
        policy: CutPolicy,
        start_index: u64,
        starts: Vec<u64>,
        limits: FiniteSegmenterLimits,
    ) -> Result<Self, Fmp4Error> {
        limits.validate()?;
        if init
            .video()
            .is_none_or(|video| video.codec != Some(VideoCodec::H264))
            || init.tracks.iter().any(|track| {
                track.kind == TrackKind::Video
                    && (track.codec != Some(VideoCodec::H264) || track.dolby_vision_config)
            })
        {
            return Err(Fmp4Error::Unsupported(
                "finite segmenter requires ordinary AVC".into(),
            ));
        }
        let mut segmenter = Self::following_encoded(init, policy, start_index, starts)?;
        segmenter.reserve_pending_fragments(limits.retained_fragments)?;
        segmenter.finite_limits = Some(limits);
        Ok(segmenter)
    }

    /// Check all final clone and publication partitions before any source
    /// fragment is copied, and retain the original pending run until success.
    pub(super) fn finish_bounded(
        &mut self,
        limits: FiniteSegmenterLimits,
    ) -> Result<Vec<Published>, Fmp4Error> {
        if self.pending.is_empty() {
            return Ok(Vec::new());
        }
        let shape = finish_budget::preflight(self, limits.split_memory_bytes)?;
        let chunks = finish_budget::chunks(self, &shape)?;
        let slots = chunks
            .len()
            .checked_mul(size_of::<Published>())
            .ok_or_else(|| {
                Fmp4Error::Unsupported("finite publication batch size overflows".into())
            })?;
        let mut batch_bytes = slots;
        for fragments in &chunks {
            let preflight = merge_budget::preflight(
                fragments,
                &self.init,
                self.retain_hevc_parameter_sets,
                limits.merge,
            )?;
            batch_bytes = batch_bytes
                .checked_add(preflight.output_bytes)
                .ok_or_else(|| {
                    Fmp4Error::Unsupported("finite publication batch size overflows".into())
                })?;
            if batch_bytes > limits.published_batch_bytes {
                return Err(Fmp4Error::Unsupported(
                    "finite publication batch exceeds its memory partition".into(),
                ));
            }
        }
        let mut published: Vec<Published> = merge_budget::reserve(chunks.len())?;
        for (offset, fragments) in chunks.iter().enumerate() {
            let index = self.next_index + offset as u64;
            let seconds = self.seconds_for(fragments, CutReason::EndOfStream);
            if let Some(strict_ticks) = self.policy.strict_max_ticks {
                let strict_seconds = strict_ticks as f64 / self.video_timescale as f64;
                if !seconds.is_finite() || seconds <= 0.0 || seconds > strict_seconds {
                    return Err(Fmp4Error::Unsupported(format!(
                        "final segment {index} has a truthful EXTINF of {seconds:.6}s, outside the fixed {strict_seconds:.3}s rolling HLS target"
                    )));
                }
            }
            let segment = merge_retaining_bounded(
                fragments,
                &self.init,
                index as u32 + 1,
                self.retain_hevc_parameter_sets,
                limits.merge,
            )?;
            published.push(Published {
                index,
                segment,
                reason: CutReason::EndOfStream,
                seconds,
            });
        }
        self.pending.clear();
        self.pending_bytes = 0;
        self.pending_ticks = 0;
        self.next_index += published.len() as u64;
        self.counts.segments += published.len() as u64;
        self.counts.tfdt_adjustments += published
            .iter()
            .map(|item| item.segment.stats.tfdt_adjustments)
            .sum::<u32>();
        Ok(published)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init() -> Init {
        Init {
            bytes: Vec::new(),
            tracks: vec![Track {
                id: 1,
                kind: TrackKind::Video,
                timescale: 24_000,
                has_edit_list: false,
                codec: Some(VideoCodec::H264),
                dolby_vision_config: false,
                nal_length_size: 4,
                default_sample_duration: 0,
                default_sample_size: 0,
                default_sample_flags: 0,
            }],
        }
    }

    fn fragment() -> Fragment {
        Fragment {
            bytes: vec![0; 16],
            mdat_payload: 8..16,
            tracks: vec![TrackFragment {
                track_id: 1,
                base_decode_time: 0,
                runs: vec![Run {
                    version: 0,
                    composition_offsets_present: false,
                    data_offset_at: None,
                    data_offset: 8,
                    samples: vec![Sample {
                        size_at: None,
                        duration: 24_000,
                        size: 8,
                        flags: 0,
                        cto: 0,
                    }],
                }],
            }],
        }
    }

    fn limits() -> FiniteSegmenterLimits {
        FiniteSegmenterLimits {
            merge: MergeMemoryLimits {
                workspace_bytes: 64 * 1024,
                output_bytes: 64 * 1024,
                max_tracks: 2,
                max_samples: 16,
            },
            prefix_memory_bytes: 64 * 1024,
            split_memory_bytes: 64 * 1024,
            published_batch_bytes: 64 * 1024,
            retained_fragments: 2,
        }
    }

    #[test]
    fn bounded_final_avc_publication_matches_legacy_bytes() {
        let policy = CutPolicy::new(1, 1, 64 * 1024, 24_000, 24_000);
        let mut legacy = Segmenter::following_encoded(init(), policy, 0, vec![0]).unwrap();
        let mut bounded =
            Segmenter::following_encoded_bounded(init(), policy, 0, vec![0], limits()).unwrap();
        legacy.pending.push(fragment());
        bounded.pending.push(fragment());
        assert_eq!(bounded.finish().unwrap(), legacy.finish().unwrap());
    }

    #[test]
    fn insufficient_split_partition_refuses_before_cloning_or_consuming_pending() {
        let policy = CutPolicy::new(1, 1, 64 * 1024, 24_000, 24_000);
        let mut narrow = limits();
        narrow.split_memory_bytes = 1;
        let mut bounded =
            Segmenter::following_encoded_bounded(init(), policy, 0, vec![0], narrow).unwrap();
        bounded.pending.push(fragment());
        let original = bounded.pending.clone();
        assert!(matches!(bounded.finish(), Err(Fmp4Error::Unsupported(_))));
        assert_eq!(bounded.pending, original);
    }
    fn muxed_init() -> Init {
        let mut init = init();
        let mut audio = init.tracks[0].clone();
        audio.id = 2;
        audio.kind = TrackKind::Audio;
        audio.timescale = 48_000;
        audio.codec = None;
        audio.nal_length_size = 0;
        init.tracks.push(audio);
        init
    }

    fn boundary_fragment() -> Fragment {
        let mut incoming = fragment();
        incoming.tracks[0].base_decode_time = 24_000;
        incoming.bytes.resize(32, 0);
        incoming.mdat_payload = 8..32;
        incoming.tracks.push(TrackFragment {
            track_id: 2,
            base_decode_time: 46_080,
            runs: vec![Run {
                version: 0,
                composition_offsets_present: false,
                data_offset_at: None,
                data_offset: 16,
                samples: vec![
                    Sample {
                        size_at: None,
                        duration: 1_024,
                        size: 4,
                        flags: 0,
                        cto: 0,
                    };
                    4
                ],
            }],
        });
        incoming
    }

    #[test]
    fn bounded_encoded_audio_prefix_matches_legacy_at_planned_avc_cut() {
        let policy = CutPolicy::new(1, 1, 64 * 1024, 24, 24_000);
        let mut finite_limits = limits();
        finite_limits.retained_fragments = 3; // prior + AAC prefix + incoming
        let mut legacy =
            Segmenter::following_encoded(muxed_init(), policy, 0, vec![0, 24_000]).unwrap();
        let mut bounded = Segmenter::following_encoded_bounded(
            muxed_init(),
            policy,
            0,
            vec![0, 24_000],
            finite_limits,
        )
        .unwrap();
        assert!(legacy.push(fragment()).unwrap().is_none());
        assert!(bounded.push(fragment()).unwrap().is_none());
        let shape = super::super::prefix_budget::preflight(
            &boundary_fragment(),
            bounded.init(),
            24_000,
            24_000,
            finite_limits.prefix_memory_bytes,
        )
        .unwrap();
        assert_eq!(shape.samples, 2);
        assert_eq!(shape.bytes, 8);
        let first_legacy = legacy.push(boundary_fragment()).unwrap().unwrap();
        let first_bounded = bounded.push(boundary_fragment()).unwrap().unwrap();
        assert_eq!(first_bounded, first_legacy);
        assert_eq!(first_bounded.index, 0);
        assert_eq!(bounded.pending[0].track(2).unwrap().sample_count(), 2);
        assert_eq!(bounded.finish().unwrap(), legacy.finish().unwrap());
    }

    #[test]
    fn too_small_prefix_partition_refuses_before_draining_aac() {
        let policy = CutPolicy::new(1, 1, 64 * 1024, 24, 24_000);
        let mut finite_limits = limits();
        finite_limits.retained_fragments = 3;
        finite_limits.prefix_memory_bytes = 1;
        let mut bounded = Segmenter::following_encoded_bounded(
            muxed_init(),
            policy,
            0,
            vec![0, 24_000],
            finite_limits,
        )
        .unwrap();
        assert!(bounded.push(fragment()).unwrap().is_none());
        let pending_before = bounded.pending.clone();
        let pending_bytes_before = bounded.pending_bytes;
        let incoming = boundary_fragment();
        let incoming_before = incoming.clone();
        assert!(matches!(
            super::super::prefix_budget::preflight(&incoming, bounded.init(), 24_000, 24_000, 1,),
            Err(Fmp4Error::Unsupported(_))
        ));
        assert_eq!(incoming, incoming_before);
        assert!(matches!(
            bounded.push(incoming),
            Err(Fmp4Error::Unsupported(_))
        ));
        assert_eq!(bounded.pending, pending_before);
        assert_eq!(bounded.pending_bytes, pending_bytes_before);
        assert_eq!(bounded.next_index, 0);
    }
}
