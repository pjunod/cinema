//! Same-input video planning checks between canonical and AV outputs.
//! This does not attest source content or authenticate range publication.

use super::{packet_clock::SourceFragmentClock, IndexRow};
use crate::fmp4::{self, Fragment, Init, VideoOnlyFragment};

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
        // The companion form: the AV output's audio may add file-type brands.
        let canonical = fmp4::copy_video_companion_init_sha256(&canonical_init)
            .map_err(|_| CompanionError::InitContract)?;
        let av = fmp4::copy_video_companion_init_sha256(&av_init)
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
    ///
    /// `canonical` may be kept without its slice data: its row and each
    /// sample's size are then the whole fragment's, derived from what was
    /// kept and converted plus what was dropped, never from `av`.
    pub fn verify<'a>(
        &mut self,
        canonical: impl Into<VideoOnlyFragment<'a>>,
        clock: SourceFragmentClock,
        av: &Fragment,
    ) -> Result<IndexRow, CompanionError> {
        if self.failed {
            return Err(CompanionError::Failed);
        }
        let result = self.verify_inner(canonical.into(), clock, av);
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn verify_inner(
        &mut self,
        canonical: VideoOnlyFragment<'_>,
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
            .fragment()
            .track(canonical_video.id)
            .ok_or(CompanionError::MissingVideo)?;
        let av_track = av.track(av_video.id).ok_or(CompanionError::MissingVideo)?;
        if canonical_track.sample_count() != av_track.sample_count()
            || canonical_track
                .samples()
                .zip(av_track.samples())
                .enumerate()
                .any(|(index, (left, right))| {
                    (
                        left.duration,
                        canonical.sample_size(index, left.size),
                        left.flags,
                        left.cto,
                    ) != (
                        right.duration,
                        u64::from(right.size),
                        right.flags,
                        right.cto,
                    )
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

    fn fixture_packet() -> crate::segplan::packet_clock::SourcePacketObservation {
        crate::segplan::packet_clock::SourcePacketObservation {
            sequence: 0,
            dts: 0,
            pts: 0,
            duration: 1,
            size: 1,
            checksum: 0,
            flags: 1,
            side_data_count: 0,
            time_base_num: 1,
            time_base_den: 1,
        }
    }
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
            first_packet: fixture_packet(),
            canonical_dts: 37,
            duration,
            next_packet_sequence: 1,
            last_muxed_dts: 0,
        };
        let row = verifier
            .verify(&fragment, first_clock, &fragment)
            .expect("verified prefix");
        assert_eq!(row.dts, 37);
        assert_eq!(row.bytes as usize, fragment.len());
        // Identical sizes and cut class do not excuse replaying the old GOP
        // where the next source clock says a later fragment must begin.
        let later_clock = SourceFragmentClock {
            first_packet: fixture_packet(),
            canonical_dts: 37 + duration,
            duration,
            next_packet_sequence: 2,
            last_muxed_dts: 0,
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

    /// A canonical fragment kept without its slice data verifies to the row
    /// of the whole one. Its sample sizes are its own, what was kept plus
    /// what was dropped, never its twin's: a canonical slice one byte
    /// shorter than the AV twin's leaves the kept bytes identical and still
    /// fails the pair.
    #[test]
    fn a_trimmed_canonical_is_verified_by_its_own_sizes() {
        let (init, fragment) = fixture();
        let video = init.video().expect("video");
        let width = usize::from(video.nal_length_size);
        let clock = SourceFragmentClock {
            first_packet: fixture_packet(),
            canonical_dts: 0,
            duration: fragment.video_duration(&init),
            next_packet_sequence: 1,
            last_muxed_dts: 0,
        };
        let verifier = || CopyCompanionVerifier::new(init.clone(), init.clone()).expect("inits");
        let whole_row = verifier()
            .verify(&fragment, clock, &fragment)
            .expect("the whole canonical verifies");
        let kept = crate::testfixtures::trimmed(&init, &fragment, 11);
        assert!(kept.held_len() < fragment.len(), "slice data was dropped");
        assert_eq!(verifier().verify(&kept, clock, &fragment), Ok(whole_row));

        let mut shorter = fragment.clone();
        let mut shortened = false;
        fmp4::rewrite_video_samples(&mut shorter, &init.tracks, video.id, |sample| {
            let length = sample[..width]
                .iter()
                .fold(0usize, |length, byte| (length << 8) | usize::from(*byte));
            let slice = (sample[width] >> 1) & 0x3f < 32 && length > 3;
            if shortened || !slice {
                return Ok(sample.to_vec());
            }
            shortened = true;
            let mut next = ((length - 1) as u32).to_be_bytes()[4 - width..].to_vec();
            next.extend_from_slice(&sample[width..width + length - 1]);
            next.extend_from_slice(&sample[width + length..]);
            Ok(next)
        })
        .expect("a canonical slice one byte shorter");
        assert!(shortened, "the fixture's samples open on a slice");
        let shorter = crate::testfixtures::trimmed(&init, &shorter, 11);
        assert_eq!(
            shorter.kept().bytes[shorter.kept().mdat_payload.clone()],
            kept.kept().bytes[kept.kept().mdat_payload.clone()],
            "only dropped slice data differs"
        );
        assert_eq!(
            verifier().verify(&shorter, clock, &fragment),
            Err(CompanionError::Samples)
        );
    }

    #[test]
    fn mismatched_companion_samples_fail_before_the_first_publication() {
        let (init, fragment) = fixture();
        let mut av = fragment.clone();
        av.tracks[0].runs[0].samples[0].size += 1;
        let clock = SourceFragmentClock {
            first_packet: fixture_packet(),
            canonical_dts: 0,
            duration: fragment.video_duration(&init),
            next_packet_sequence: 1,
            last_muxed_dts: 0,
        };
        let mut verifier =
            CopyCompanionVerifier::new(init.clone(), init).expect("matching decoder contracts");
        assert_eq!(
            verifier.verify(&fragment, clock, &av),
            Err(CompanionError::Samples)
        );
    }
}
