//! Retry delay policy shared by the daemon and its bounded protocol model.

/// Return the retry backoff with deterministic jitter from the raw job-ID bytes.
///
/// The signed attempt count is cast directly to `u64` for the wrapping byte fold;
/// negative counts retain the same fallback base and cast semantics as the daemon.
pub fn retry_delay_ms(id: &str, failed_attempts: i64) -> i64 {
    let base = match failed_attempts {
        0 => 5_000,
        1 => 30_000,
        2 => 120_000,
        _ => 600_000,
    };
    let jitter = id.bytes().fold(failed_attempts as u64, |seed, byte| {
        seed.wrapping_mul(31).wrapping_add(u64::from(byte))
    }) % (base as u64 / 5);
    base + jitter as i64
}

#[cfg(test)]
mod tests {
    use super::retry_delay_ms;

    #[test]
    fn retry_bases_preserve_attempt_thresholds() {
        assert_eq!(retry_delay_ms("", 0), 5_000);
        assert_eq!(retry_delay_ms("", 1), 30_001);
        assert_eq!(retry_delay_ms("", 2), 120_002);
        assert_eq!(retry_delay_ms("", 3), 600_003);
    }

    #[test]
    fn raw_id_bytes_and_wrapping_signed_seed_preserve_jitter() {
        let id = "00000000-0000-4000-8000-000000000001";
        assert_eq!(retry_delay_ms(id, 0), 5_101);
        assert_eq!(retry_delay_ms(id, 2), 130_759);
        assert_eq!(retry_delay_ms("é", -1), 605_253);
        assert_eq!(retry_delay_ms("a", -1), 600_066);
        assert_eq!(retry_delay_ms("", -1), 711_615);
        assert_eq!(retry_delay_ms("", i64::MIN), 655_808);
    }
}
