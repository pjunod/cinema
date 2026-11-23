use super::*;
fn preroll_index() -> FragmentIndex {
    let mut index = FragmentIndex::new(
        1000,
        (0..4)
            .map(|at| plurx_core::segplan::IndexRow {
                dts: at * 100,
                first_cto: None,
                duration: 100,
                bytes: 1,
                video_bytes: 1,
                class: plurx_core::fmp4::CutClass::CleanIdr,
            })
            .collect(),
        "a".repeat(64),
        plurx_core::segplan::SourceIdentity::new(1, 1, "fitted-v4"),
    );
    index.dv_refit_proofs = Some((1..=4).map(|code| preroll_proof(code, 7)).collect());
    index
}
#[test]
fn fitted_preroll_discards_only_identified_pre_target_clock_damage() {
    let index = preroll_index();
    let full = [(2, preroll_proof(1, 7), 10_000)];
    assert_eq!(
        proved_fitted_preroll_start(&index, 0, 0, &full, false),
        Some((0, 0, 10_000, -10_000)),
        "without a preroll discard, retain leading audio-only fragments"
    );
    let raw = [
        (0, preroll_proof(1, 9), 10_000),
        (1, preroll_proof(2, 7), 10_083),
        (2, preroll_proof(3, 7), 10_183),
    ];
    assert_eq!(
        proved_fitted_preroll_start(&index, 0, 1, &raw, true),
        Some((1, 1, 10_083, -9_983))
    );
    assert!(
        proved_fitted_preroll_start(&index, 0, 0, &raw, true).is_none(),
        "cannot discard target row"
    );
    assert!(
        proved_fitted_preroll_start(&index, 0, 1, &raw, false).is_none(),
        "whole playback cannot discard"
    );
    let mut wrong_picture = raw;
    wrong_picture[0].1 = preroll_proof(9, 9);
    assert!(
        proved_fitted_preroll_start(&index, 0, 1, &wrong_picture, true).is_none(),
        "skipped pictures also require identity"
    );
}
#[test]
fn fitted_preroll_keeps_every_served_row_clock_and_prefix_bound() {
    let index = preroll_index();
    let raw = [
        (0, preroll_proof(1, 9), 10_000),
        (1, preroll_proof(2, 7), 10_083),
        (2, preroll_proof(3, 9), 10_183),
    ];
    assert!(
        proved_fitted_preroll_start(&index, 0, 1, &raw, true).is_none(),
        "later served cadence cannot differ"
    );
    let target_damage = [
        (0, preroll_proof(1, 9), 10_000),
        (1, preroll_proof(2, 9), 10_083),
        (2, preroll_proof(3, 7), 10_183),
    ];
    assert!(
        proved_fitted_preroll_start(&index, 0, 1, &target_damage, true).is_none(),
        "damage intersecting target cannot be discarded"
    );
    let drift = [
        (0, preroll_proof(1, 9), 10_000),
        (1, preroll_proof(2, 7), 10_083),
        (2, preroll_proof(3, 7), 10_184),
    ];
    assert!(
        proved_fitted_preroll_start(&index, 0, 1, &drift, true).is_none(),
        "fixed translation includes every served row"
    );
    let over_bound = [
        (0, preroll_proof(1, 9), 10_000),
        (1, preroll_proof(2, 9), 10_100),
        (2, preroll_proof(3, 9), 10_200),
        (3, preroll_proof(4, 7), 10_300),
    ];
    assert!(
        proved_fitted_preroll_start(&index, 0, 3, &over_bound, true).is_none(),
        "at most two preroll fragments"
    );
}

fn preroll_proof(coded: u8, cadence: u8) -> plurx_core::segplan::refit_proof::FittedFragmentProof {
    let mut bytes = [0u8; 72];
    bytes[..32].fill(coded);
    bytes[32..64].fill(cadence);
    serde_json::from_value(serde_json::json!(hex::encode(bytes))).expect("v4 proof")
}
