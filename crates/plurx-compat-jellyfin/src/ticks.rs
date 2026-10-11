//! Checked conversion between Jellyfin's 100 ns ticks and native milliseconds.
use serde::{Deserialize, Serialize};
pub const TICKS_PER_MILLISECOND: i64 = 10_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "i64", into = "i64")]
pub struct Ticks(i64);
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("negative or overflowing playback time")]
pub struct InvalidTime;
impl Ticks {
    pub fn from_milliseconds(value: i64) -> Result<Self, InvalidTime> {
        if value < 0 {
            return Err(InvalidTime);
        }
        value
            .checked_mul(TICKS_PER_MILLISECOND)
            .map(Self)
            .ok_or(InvalidTime)
    }
    pub fn milliseconds(self) -> i64 {
        self.0 / TICKS_PER_MILLISECOND
    }
    pub fn value(self) -> i64 {
        self.0
    }
}
impl TryFrom<i64> for Ticks {
    type Error = InvalidTime;
    fn try_from(value: i64) -> Result<Self, Self::Error> {
        if value < 0 {
            Err(InvalidTime)
        } else {
            Ok(Self(value))
        }
    }
}
impl From<Ticks> for i64 {
    fn from(value: Ticks) -> Self {
        value.0
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ticks_preserve_zero_reject_negative_and_check_runtime_overflow() {
        assert_eq!(Ticks::from_milliseconds(0).expect("zero").value(), 0);
        assert_eq!(
            Ticks::from_milliseconds(123).expect("time").value(),
            1_230_000
        );
        assert_eq!(
            Ticks::try_from(10_001)
                .expect("sub-ms ticks")
                .milliseconds(),
            1
        );
        let maximum = i64::MAX / TICKS_PER_MILLISECOND;
        assert!(Ticks::from_milliseconds(maximum).is_ok());
        assert!(Ticks::from_milliseconds(maximum + 1).is_err());
        assert!(Ticks::from_milliseconds(-1).is_err());
        assert!(Ticks::try_from(-1).is_err());
        assert!(serde_json::from_str::<Ticks>("-1").is_err());
        assert_eq!(
            serde_json::from_str::<Option<Ticks>>("null").expect("missing"),
            None
        );
        assert_eq!(
            serde_json::from_str::<Option<Ticks>>("0").expect("zero"),
            Some(Ticks(0))
        );
    }
}
