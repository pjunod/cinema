//! Same-input video planning checks between canonical and AV outputs.
//! This does not attest source content or authenticate range publication.

use super::{packet_clock::SourceFragmentClock, IndexRow};
use crate::fmp4::{self, Fragment, Init};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompanionError {
    InitContract,
    MissingVideo,
    Samples,
    Timeline,
    Measurement(&'static str),
    Failed,
}

/// Construct after applying the same init normalization/promotion and sample
/// conversion to both outputs. Per-range origins remain fixed for its life.
/// Audio/container overhead is intentionally absent from canonical row bytes.
pub struct CopyCompanionVerifier {
    canonical_init: Init,
    av_init: Init,
    origins: Option<(u64, u64, u64)>,
    next_source_tick: Option<u64>,
    failed: bool,
}

impl CopyCompanionVerifier {
    pub fn new(canonical_init: Init, av_init: Init) -> Result<Self, CompanionError> {
        let canonical = fmp4::copy_video_init_contract_sha256(&canonical_init)
            .map_err(|_| CompanionError::InitContract)?;
        let av = fmp4::copy_video_init_contract_sha256(&av_init)
            .map_err(|_| CompanionError::InitContract)?;
        if canonical != av || canonical_init.tracks.len() != 1 {
            return Err(CompanionError::InitContract);
        }
        Ok(Self {
            canonical_init,
            av_init,
            origins: None,
            next_source_tick: None,
            failed: false,
        })
    }

    /// Verify every video fragment before the corresponding AV segment can
    /// become servable. `clock` was associated before conversion; row byte
    /// measurements below are after conversion. A good prefix never disables
    /// later checks. The caller discards audio-only AV flushes separately.
    pub fn verify(
        &mut self,
        canonical: &Fragment,
        clock: SourceFragmentClock,
        av: &Fragment,
    ) -> Result<IndexRow, CompanionError> {
        if self.failed {
            return Err(CompanionError::Failed);
        }
        let result = self.verify_inner(canonical, clock, av);
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn verify_inner(
        &mut self,
        canonical: &Fragment,
        clock: SourceFragmentClock,
        av: &Fragment,
    ) -> Result<IndexRow, CompanionError> {
        let mut row = IndexRow::from_fragment(canonical, &self.canonical_init)
            .map_err(CompanionError::Measurement)?;
        let canonical_video = self
            .canonical_init
            .video()
            .ok_or(CompanionError::MissingVideo)?;
        let av_video = self.av_init.video().ok_or(CompanionError::MissingVideo)?;
        let canonical_track = canonical
            .track(canonical_video.id)
            .ok_or(CompanionError::MissingVideo)?;
        let av_track = av.track(av_video.id).ok_or(CompanionError::MissingVideo)?;
        if canonical_track.sample_count() != av_track.sample_count()
            || canonical_track
                .samples()
                .zip(av_track.samples())
                .any(|(left, right)| {
                    (left.duration, left.size, left.flags, left.cto)
                        != (right.duration, right.size, right.flags, right.cto)
                })
            || fmp4::classify(av, &self.av_init) != row.class
        {
            return Err(CompanionError::Samples);
        }
        let origins =
            self.origins
                .unwrap_or((row.dts, av_track.base_decode_time, clock.canonical_dts));
        let source_delta = i128::from(clock.canonical_dts) - i128::from(origins.2);
        if row.duration != clock.duration
            || self
                .next_source_tick
                .is_some_and(|next| next != clock.canonical_dts)
            || source_delta != i128::from(row.dts) - i128::from(origins.0)
            || source_delta != i128::from(av_track.base_decode_time) - i128::from(origins.1)
        {
            return Err(CompanionError::Timeline);
        }
        let next = clock
            .canonical_dts
            .checked_add(clock.duration)
            .ok_or(CompanionError::Timeline)?;
        row.dts = clock.canonical_dts;
        self.origins = Some(origins);
        self.next_source_tick = Some(next);
        Ok(row)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fmp4::{FragmentReader, Unit};

    fn fixture() -> (Init, Fragment) {
        let mut reader = FragmentReader::new();
        reader.push(include_bytes!(
            "../../tests/fixtures/dv-runtime/authored.mp4"
        ));
        let Some(Unit::Init(init)) = reader.next_unit().expect("fixture init") else {
            panic!("init");
        };
        let Some(Unit::Fragment(fragment)) = reader.next_unit().expect("fixture fragment") else {
            panic!("fragment");
        };
        (init, fragment)
    }

    #[test]
    fn a_verified_prefix_cannot_hide_a_later_duplicate_or_restart_the_verifier() {
        let (init, fragment) = fixture();
        let duration = fragment.video_duration(&init);
        let mut verifier =
            CopyCompanionVerifier::new(init.clone(), init).expect("matching decoder contracts");
        let first_clock = SourceFragmentClock {
            canonical_dts: 37,
            duration,
            next_packet_sequence: 1,
        };
        let row = verifier
            .verify(&fragment, first_clock, &fragment)
            .expect("verified prefix");
        assert_eq!(row.dts, 37);
        assert_eq!(row.bytes as usize, fragment.len());
        // Identical sizes and cut class do not excuse replaying the old GOP
        // where the next source clock says a later fragment must begin.
        let later_clock = SourceFragmentClock {
            canonical_dts: 37 + duration,
            duration,
            next_packet_sequence: 2,
        };
        assert_eq!(
            verifier.verify(&fragment, later_clock, &fragment),
            Err(CompanionError::Timeline)
        );
        assert_eq!(
            verifier.verify(&fragment, first_clock, &fragment),
            Err(CompanionError::Failed)
        );
    }

    #[test]
    fn mismatched_companion_samples_fail_before_the_first_publication() {
        let (init, fragment) = fixture();
        let mut av = fragment.clone();
        av.tracks[0].runs[0].samples[0].size += 1;
        let clock = SourceFragmentClock {
            canonical_dts: 0,
            duration: fragment.video_duration(&init),
            next_packet_sequence: 1,
        };
        let mut verifier =
            CopyCompanionVerifier::new(init.clone(), init).expect("matching decoder contracts");
        assert_eq!(
            verifier.verify(&fragment, clock, &av),
            Err(CompanionError::Samples)
        );
    }
}
