use super::recovery_reservation_from_row;
use crate::playback_principal::PlaybackPrincipal;

#[test]
fn sharing_recovery_converter_preserves_complete_owner_and_refuses_invalid_local() {
    let convert = |principal| {
        recovery_reservation_from_row(
            principal,
            "playback",
            "epoch",
            "incarnation".to_owned(),
            1,
            1,
            "a".repeat(64),
            "b".repeat(64),
            None,
            "reserved",
            1,
            1,
        )
    };
    let principal = PlaybackPrincipal::sharing(
        uuid::Uuid::parse_str("00000000-0000-4000-a000-000000000001").expect("grant UUID"),
        &"a".repeat(64),
    )
    .expect("viewer key");
    assert_eq!(
        convert(principal.clone())
            .expect("valid shared row")
            .principal,
        principal
    );
    assert!(convert(PlaybackPrincipal::LocalUser { user_id: 0 }).is_err());
    assert!(convert(PlaybackPrincipal::LocalUser { user_id: -1 }).is_err());
}
