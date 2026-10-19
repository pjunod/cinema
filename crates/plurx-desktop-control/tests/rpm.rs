#![cfg(unix)]
#[test]
fn rpm_epoch_zero_is_equivalent_to_absent_but_nonzero_must_be_signed() {
    use plurx_desktop_control::package::rpm_versions_match;
    assert!(rpm_versions_match("0:0.7.10-1", "0.7.10-1"));
    assert!(rpm_versions_match("0.7.10-1", "0:0.7.10-1"));
    assert!(!rpm_versions_match("1:0.7.10-1", "0.7.10-1"));
    assert!(!rpm_versions_match("0:0.7.10-2", "0.7.10-1"));
    assert!(rpm_versions_match("1:0.7.10-1", "1:0.7.10-1"));
}
