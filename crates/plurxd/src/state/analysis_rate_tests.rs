/// A resumed attestation reports 60 GB in its first second. Those bytes
/// were read by earlier attempts. Counting them as this attempt's rate
/// would show an ETA of seconds for minutes of real work.
#[test]
fn a_resumed_attempt_is_timed_from_its_resume_point() {
    let gb = 1_000_000_000_u64;
    let started = 1_000_i64;
    // Resumed at 60 GB at t=1 s, then read 1 GB in the next 10 s.
    let (rate, eta) = super::analysis_rate(61 * gb, 80 * gb, (60 * gb, started), started + 10_000);
    assert_eq!(rate, 100_000_000, "1 GB in 10 s, not 61 GB in 10 s");
    assert_eq!(eta, Some(190_000), "19 GB left at 100 MB/s");
}

#[test]
fn without_a_report_the_rate_is_measured_from_the_start() {
    let (rate, eta) = super::analysis_rate(0, 100, (0, 5_000), 6_000);
    assert_eq!((rate, eta), (0, None));
    let (rate, eta) = super::analysis_rate(50, 100, (0, 5_000), 6_000);
    assert_eq!((rate, eta), (50, Some(1_000)));
    let (rate, _) = super::analysis_rate(50, 100, (50, 6_000), 6_000);
    assert_eq!(rate, 0, "no time since the origin is no measurement");
}
