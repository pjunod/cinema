use super::*;
#[test]
fn custody_settlement_accepts_only_exact_supported_winners_and_preserves_refusals() {
    let snapshot = |schema_version, absent, exact| CustodyMigrationSnapshot {
        compatibility: CompatibilityRow {
            schema_version,
            protocol_min: AUTH_PROTOCOL_MIN,
            protocol_max: AUTH_PROTOCOL_MIN,
        },
        absent,
        exact,
    };
    assert!(!custody_snapshot_settled(&snapshot(80, 1, 0)).expect("actual absent predecessor"));
    for version in [81, 82] {
        assert!(custody_snapshot_settled(&snapshot(version, 0, 1)).expect("exact supported winner"));
        for (absent, exact) in [(1, 0), (0, 0), (1, 1)] {
            assert!(
                custody_snapshot_settled(&snapshot(version, absent, exact)).is_err(),
                "missing or inconsistent winner shape must refuse"
            );
        }
    }
    for version in [69, 70, 71, 73, 79, 80, 83] {
        assert!(
            custody_snapshot_settled(&snapshot(version, 0, 1)).is_err(),
            "private/unsupported markers or premature custody cannot be adopted"
        );
    }
    let mut incompatible = snapshot(81, 0, 1);
    incompatible.compatibility.protocol_max = AUTH_PROTOCOL_MAX + 1;
    assert!(
        custody_snapshot_settled(&incompatible).is_err(),
        "exact shape cannot override incompatible protocols"
    );
}
