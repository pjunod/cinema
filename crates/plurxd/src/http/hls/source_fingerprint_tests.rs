use super::source_playback_fingerprint;
#[test]
fn source_fingerprint_freezes_native_choice_and_normalizes_disabled_choice() {
    let base = "a".repeat(64);
    let copy = source_playback_fingerprint(&base, false, None);
    assert_eq!(copy, source_playback_fingerprint(&base, false, Some(0)));
    let native = source_playback_fingerprint(&base, true, None);
    let first = source_playback_fingerprint(&base, true, Some(0));
    let second = source_playback_fingerprint(&base, true, Some(1));
    assert_ne!(copy, native);
    assert_ne!(native, first);
    assert_ne!(first, second);
    assert!(first.len() == 64 && first.bytes().all(|b| b.is_ascii_hexdigit()));
}
