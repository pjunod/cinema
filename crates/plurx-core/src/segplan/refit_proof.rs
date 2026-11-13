//! Fitted-copy pre-conversion identity. This proves equivalence to each
//! canonical index fragment, not correspondence to an unobserved source PTS
//! census. Source-clock establishment and source authority remain separate.
use crate::fmp4::{self, Fragment, Init};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};

/// Ordered coded pictures and exact per-picture cadence before To81/F+.
/// Base DTS is checked independently under one generation-wide translation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FittedFragmentProof([u8; 32]);

impl Serialize for FittedFragmentProof {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&hex::encode(self.0))
    }
}
impl<'de> Deserialize<'de> for FittedFragmentProof {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        let bytes = hex::decode(text).map_err(serde::de::Error::custom)?;
        Ok(Self(bytes.try_into().map_err(|_| {
            serde::de::Error::custom("fitted proof must be SHA256")
        })?))
    }
}

impl FittedFragmentProof {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn observe(fragment: &Fragment, init: &Init) -> Result<Self, String> {
        let video = init.video().ok_or("fitted proof has no video init")?;
        let track = fragment
            .track(video.id)
            .ok_or("fitted proof has no video fragment")?;
        if video.timescale == 0 || track.sample_count() == 0 {
            return Err("fitted proof has no video cadence".into());
        }
        let mut hash = Sha256::new();
        hash.update(b"plurx/fitted-copy/ordered-coded-cadence/v3\0");
        hash.update(video.timescale.to_be_bytes());
        hash.update(
            u64::try_from(track.sample_count())
                .map_err(|_| "fitted sample count overflows")?
                .to_be_bytes(),
        );
        for run in &track.runs {
            let mut at = run.data_offset;
            for sample in &run.samples {
                if sample.duration == 0 {
                    return Err("fitted proof has zero-duration sample".into());
                }
                let end = at
                    .checked_add(sample.size as usize)
                    .ok_or("fitted sample extent overflows")?;
                let bytes = fragment
                    .bytes
                    .get(at..end)
                    .ok_or("fitted sample extent is invalid")?;
                hash.update(sample.duration.to_be_bytes());
                hash.update(sample.cto.to_be_bytes());
                hash.update(
                    fmp4::hevc_cbs_annex_b_sample_hash(bytes, video.nal_length_size)
                        .map_err(|error| error.to_string())?,
                );
                at = end;
            }
        }
        Ok(Self(hash.finalize().into()))
    }
}

/// Check one raw fragment before any curve conversion. The chosen candidate
/// fixes `translation = index DTS - production DTS` for the entire generation.
pub fn verify_fitted_fragment(
    index: &super::FragmentIndex,
    row: usize,
    fragment: &Fragment,
    init: &Init,
    translation: i128,
) -> Result<(), String> {
    let proofs = index
        .dv_refit_proofs
        .as_ref()
        .filter(|proofs| proofs.len() == index.rows.len())
        .ok_or("fitted index lacks complete pre-conversion proofs")?;
    let expected = index
        .rows
        .get(row)
        .ok_or("fitted output exceeded its indexed rows")?;
    let video = init.video().ok_or("fitted output has no video init")?;
    let track = fragment
        .track(video.id)
        .ok_or("fitted output has no video fragment")?;
    if video.timescale != index.timescale
        || i128::from(track.base_decode_time).checked_add(translation)
            != Some(i128::from(expected.dts))
    {
        return Err("fitted output changed its fixed decode-time translation".into());
    }
    if FittedFragmentProof::observe(fragment, init)? != proofs[row] {
        return Err("fitted output changed ordered coded pictures or sample cadence".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fmp4::{FragmentReader, Unit};
    use crate::segplan::{FragmentIndex, IndexRow, SourceIdentity};

    fn fixture() -> (Init, Fragment) {
        let mut reader = FragmentReader::new();
        reader.push(include_bytes!(
            "../../tests/fixtures/dv-runtime/authored.mp4"
        ));
        let Some(Unit::Init(init)) = reader.next_unit().expect("init") else {
            panic!("init missing");
        };
        while let Some(unit) = reader.next_unit().expect("fragment") {
            if let Unit::Fragment(fragment) = unit {
                return (init, fragment);
            }
        }
        panic!("fragment missing");
    }
    fn index(init: &Init, fragment: &Fragment) -> FragmentIndex {
        let mut index = FragmentIndex::new(
            init.video().expect("video").timescale,
            vec![IndexRow::from_fragment(fragment, init).expect("row")],
            hex::encode(Sha256::digest(&init.bytes)),
            SourceIdentity::new(1, 1, "fitted-v3"),
        );
        index.dv_refit_proofs = Some(vec![
            FittedFragmentProof::observe(fragment, init).expect("proof")
        ]);
        index
    }

    #[test]
    fn fitted_proof_roundtrips_and_legacy_none_stays_absent() {
        let (init, fragment) = fixture();
        let index = index(&init, &fragment);
        let encoded = serde_json::to_vec(&index).expect("JSON");
        let decoded: FragmentIndex = serde_json::from_slice(&encoded).expect("roundtrip");
        assert_eq!(decoded, index);
        let source = "a".repeat(64);
        let pipeline = "b".repeat(64);
        let blob = crate::store::encode_cluster_fragment_index_blob(&index, &source, &pipeline)
            .expect("portable proofs");
        let decoded = crate::store::decode_cluster_fragment_index_blob(&blob, &source, &pipeline)
            .expect("portable roundtrip");
        assert_eq!(decoded.dv_refit_proofs, index.dv_refit_proofs);
        let mut legacy = index;
        legacy.dv_refit_proofs = None;
        let value = serde_json::to_value(&legacy).expect("legacy JSON");
        assert!(value.get("dv_refit_proofs").is_none());
        let decoded: FragmentIndex = serde_json::from_value(value).expect("old rows load");
        assert!(decoded.dv_refit_proofs.is_none());
        assert!(verify_fitted_fragment(&decoded, 0, &fragment, &init, 0).is_err());
    }

    #[test]
    fn fitted_proof_requires_cadence_cardinality_and_one_fixed_base_translation() {
        let (init, fragment) = fixture();
        let mut index = index(&init, &fragment);
        let video = init.video().expect("video").id;
        let mut later = fragment.clone();
        let span = index.rows[0].duration;
        later
            .tracks
            .iter_mut()
            .find(|track| track.track_id == video)
            .expect("track")
            .base_decode_time += span;
        index
            .rows
            .push(IndexRow::from_fragment(&later, &init).expect("later row"));
        index
            .dv_refit_proofs
            .as_mut()
            .expect("proofs")
            .push(FittedFragmentProof::observe(&later, &init).expect("later proof"));
        verify_fitted_fragment(&index, 1, &later, &init, 0).expect("later raw row");
        let mut shifted = later.clone();
        shifted
            .tracks
            .iter_mut()
            .find(|track| track.track_id == video)
            .expect("track")
            .base_decode_time += 777;
        verify_fitted_fragment(&index, 1, &shifted, &init, -777).expect("one allowed translation");
        assert!(verify_fitted_fragment(&index, 1, &shifted, &init, -776).is_err());
        for kind in 0..3 {
            let mut changed = later.clone();
            let samples = &mut changed
                .tracks
                .iter_mut()
                .find(|track| track.track_id == video)
                .expect("track")
                .runs[0]
                .samples;
            match kind {
                0 => samples[0].duration += 1,
                1 => samples[0].cto += 1,
                _ => {
                    samples.pop();
                }
            }
            assert!(verify_fitted_fragment(&index, 1, &changed, &init, 0).is_err());
        }
        assert!(verify_fitted_fragment(&index, 2, &later, &init, 0).is_err());
        index.dv_refit_proofs.as_mut().expect("proofs").pop();
        assert!(verify_fitted_fragment(&index, 0, &fragment, &init, 0).is_err());
    }

    fn with_rpu(init: &Init, fragment: &Fragment, rpu: &[u8]) -> Fragment {
        let video = init.video().expect("video");
        let width = usize::from(video.nal_length_size);
        let mut out = fragment.clone();
        fmp4::rewrite_video_samples(&mut out, &init.tracks, video.id, |sample| {
            let mut next = Vec::new();
            fmp4::visit_length_prefixed_hevc_nals(
                sample,
                video.nal_length_size,
                |framed, kind| {
                    if kind != Some(62) {
                        next.extend_from_slice(framed);
                    }
                },
            )?;
            next.extend_from_slice(&(rpu.len() as u64).to_be_bytes()[8 - width..]);
            next.extend_from_slice(rpu);
            Ok(next)
        })
        .expect("replace valid RPU");
        out
    }

    #[test]
    fn equal_length_rpus_with_different_real_curves_cannot_pass_the_raw_guard() {
        use crate::transcode::dv_refit::{apply_to_rpu, servable, RefitCurves};
        use dolby_vision::rpu::{dovi_rpu::DoviRpu, ConversionMode};
        let mut variants = Vec::new();
        for lift in 1..=32 {
            let mut curves = RefitCurves::identity();
            for piece in &mut curves.luma {
                piece[0] += f64::from(lift) / 4095.0;
            }
            let mut rpu =
                DoviRpu::parse_unspec62_nalu(&crate::testfixtures::p7_rpu()).expect("source RPU");
            rpu.convert_with_mode(ConversionMode::To81).expect("To81");
            apply_to_rpu(&mut rpu, &servable(&curves).expect("safe curve")).expect("write curves");
            let bytes = rpu.write_hevc_unspec62_nalu().expect("RPU bytes");
            DoviRpu::parse_unspec62_nalu(&bytes).expect("changed curve still valid");
            if let Some(prior) = variants
                .iter()
                .find(|prior: &&Vec<u8>| prior.len() == bytes.len() && **prior != bytes)
            {
                let (init, fragment) = fixture();
                let a = with_rpu(&init, &fragment, prior);
                let b = with_rpu(&init, &fragment, &bytes);
                assert_eq!(
                    IndexRow::from_fragment(&a, &init).expect("a").video_bytes,
                    IndexRow::from_fragment(&b, &init).expect("b").video_bytes
                );
                let index = index(&init, &a);
                verify_fitted_fragment(&index, 0, &a, &init, 0).expect("original coded RPU");
                assert!(verify_fitted_fragment(&index, 0, &b, &init, 0).is_err());
                return;
            }
            variants.push(bytes);
        }
        panic!("fixture must contain equal-length distinct real curve RPUs");
    }
}

#[cfg(test)]
mod ordered_sample_tests {
    use super::*;
    use crate::fmp4::{FragmentReader, Unit};
    #[test]
    fn ordered_payload_proof_rejects_reordered_pictures_with_unchanged_total_bytes() {
        let mut reader = FragmentReader::new();
        reader.push(include_bytes!(
            "../../tests/fixtures/dv-runtime/authored.mp4"
        ));
        let Some(Unit::Init(init)) = reader.next_unit().expect("init") else {
            panic!("missing init");
        };
        let fragment = loop {
            match reader.next_unit().expect("fragment") {
                Some(Unit::Fragment(fragment)) => break fragment,
                Some(_) => {}
                None => panic!("missing fragment"),
            }
        };
        let video = init.video().expect("video");
        let track = fragment.track(video.id).expect("track");
        let mut payloads = Vec::new();
        for run in &track.runs {
            let mut at = run.data_offset;
            for sample in &run.samples {
                payloads.push(fragment.bytes[at..at + sample.size as usize].to_vec());
                at += sample.size as usize;
            }
        }
        assert!(payloads.len() >= 2);
        payloads.swap(0, 1);
        let mut replacements = payloads.into_iter();
        let mut swapped = fragment.clone();
        fmp4::rewrite_video_samples(&mut swapped, &init.tracks, video.id, |_| {
            Ok(replacements
                .next()
                .expect("one replacement per coded picture"))
        })
        .expect("same valid picture payloads");
        assert_eq!(
            crate::segplan::IndexRow::from_fragment(&fragment, &init)
                .expect("original")
                .video_bytes,
            crate::segplan::IndexRow::from_fragment(&swapped, &init)
                .expect("swapped")
                .video_bytes
        );
        assert_ne!(
            FittedFragmentProof::observe(&fragment, &init).expect("original proof"),
            FittedFragmentProof::observe(&swapped, &init).expect("reordered valid sample proof")
        );
    }
}
