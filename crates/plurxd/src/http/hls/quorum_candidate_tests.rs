use super::candidate_refusal_reason;
use plurx_core::playback::candidate::{CandidateId, CandidateRoute, QualityCandidate};
#[test]
fn candidate_refusals_distinguish_authority_identity_decoder_and_route() {
    let requested = CandidateId([1; 16]);
    assert_eq!(
        candidate_refusal_reason(&[], Some(requested), Some(1080), true),
        "catalogue_authority_unavailable"
    );
    assert_eq!(
        candidate_refusal_reason(&[], Some(requested), Some(1080), false),
        "identity_absent"
    );
    assert_eq!(
        candidate_refusal_reason(&[], None, Some(1080), false),
        "encode_route_unavailable"
    );
    let row = QualityCandidate {
        processed_dv: false,
        id: requested,
        recipe_digest: [0; 32],
        route: CandidateRoute::Encode,
        planned_codec: None,
        normalized_geometry: true,
        width: 1920,
        height: 1080,
        target_height: 1080,
        average_bps: None,
        peak_bps: None,
        grade: Default::default(),
        decoder_compatible: false,
        complete_cache: false,
        sustainable: true,
    };
    assert_eq!(
        candidate_refusal_reason(
            std::slice::from_ref(&row),
            Some(requested),
            Some(1080),
            true
        ),
        "decoder_incompatible"
    );
    assert_eq!(
        candidate_refusal_reason(&[row], Some(CandidateId([2; 16])), Some(1080), true),
        "catalogue_authority_unavailable"
    );
}
