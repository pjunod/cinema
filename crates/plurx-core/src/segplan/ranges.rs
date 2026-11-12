//! Pure sparse index observations. These types do not authenticate source,
//! grant, origin or EOF evidence; the caller must establish that authority.

use super::IndexRow;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeError {
    InvalidClock,
    NonIntegralTime,
    TimeOverflow,
    NegativeTime,
    InvalidBinding,
    BindingMismatch,
    EmptyBatch,
    InvalidRows,
    ConflictingRows,
    ConflictingBoundary,
    RowLimit,
}

/// An observed packet clock, never a presentation timestamp or source proof.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceTimestamp {
    pub dts: i64,
    pub time_base_num: u32,
    pub time_base_den: u32,
}

fn gcd(mut a: u128, mut b: u128) -> u128 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

fn fraction(time: SourceTimestamp) -> Result<(bool, u128, u128), RangeError> {
    if time.time_base_num == 0 || time.time_base_den == 0 {
        return Err(RangeError::InvalidClock);
    }
    let numerator = u128::from(time.dts.unsigned_abs()) * u128::from(time.time_base_num);
    let denominator = u128::from(time.time_base_den);
    let divisor = gcd(numerator, denominator);
    Ok((time.dts < 0, numerator / divisor, denominator / divisor))
}

/// Map source decode time to canonical index ticks, exactly:
/// `(packet_time - whole_title_origin_time) * timescale + canonical_title_start`.
/// The origin and canonical start are caller-established observations, not
/// authenticated evidence. Nonintegral ticks are refused rather than rounded.
pub fn normalize_to_index_ticks(
    packet: SourceTimestamp,
    whole_title_origin: SourceTimestamp,
    canonical_title_start: u64,
    timescale: u32,
) -> Result<u64, RangeError> {
    if timescale == 0 {
        return Err(RangeError::InvalidClock);
    }
    let (left_negative, left, left_den) = fraction(packet)?;
    let (right_negative, right, right_den) = fraction(whole_title_origin)?;
    let common = gcd(left_den, right_den);
    let left = left
        .checked_mul(right_den / common)
        .ok_or(RangeError::TimeOverflow)?;
    let right = right
        .checked_mul(left_den / common)
        .ok_or(RangeError::TimeOverflow)?;
    let denominator = (left_den / common) * right_den;
    // Subtraction in signed magnitude avoids overflowing signed intermediates
    // when cancellation or denominator reduction makes the result representable.
    let (negative, numerator) = if left_negative != right_negative {
        (
            left_negative,
            left.checked_add(right).ok_or(RangeError::TimeOverflow)?,
        )
    } else if left >= right {
        (left_negative, left - right)
    } else {
        (!left_negative, right - left)
    };
    let common = gcd(numerator, denominator);
    let numerator = numerator / common;
    let denominator = denominator / common;
    let scale_common = gcd(u128::from(timescale), denominator);
    if denominator / scale_common != 1 {
        return Err(RangeError::NonIntegralTime);
    }
    let magnitude = numerator
        .checked_mul(u128::from(timescale) / scale_common)
        .ok_or(RangeError::TimeOverflow)?;
    let magnitude = u64::try_from(magnitude).map_err(|_| RangeError::TimeOverflow)?;
    if negative {
        canonical_title_start
            .checked_sub(magnitude)
            .ok_or(RangeError::NegativeTime)
    } else {
        canonical_title_start
            .checked_add(magnitude)
            .ok_or(RangeError::TimeOverflow)
    }
}

/// Opaque caller-established generation/contract binding. Equality is not a
/// source attestation, and this accumulator never interprets these strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeBinding {
    pub source_generation: String,
    pub contract: String,
}

/// Caller-observed physical boundaries of this batch. `eof` alone does not
/// establish complete coverage; it only fixes the final row's end tick.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RangeBoundaries {
    pub begins_title: bool,
    pub eof: bool,
}

/// The first span this accumulator has not observed. `start` is the end tick
/// of the rows before it, or None when the title's beginning is unobserved.
/// `end` is the next observed row's first tick, or None when only EOF follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RangeGap {
    pub start: Option<u64>,
    pub end: Option<u64>,
}

/// Sorted nonoverlapping canonical rows with gaps allowed between batches.
/// Storage is bounded by `max_rows`; insertion is atomic on all refusals.
#[derive(Debug)]
pub struct SparseIndexRows {
    binding: RangeBinding,
    timescale: u32,
    title_start: u64,
    max_rows: usize,
    rows: Vec<IndexRow>,
    observed_beginning: bool,
    eof_end: Option<u64>,
}

fn row_end(row: &IndexRow) -> Result<u64, RangeError> {
    if row.duration == 0 {
        return Err(RangeError::InvalidRows);
    }
    row.dts
        .checked_add(row.duration)
        .ok_or(RangeError::InvalidRows)
}

impl SparseIndexRows {
    pub fn new(
        binding: RangeBinding,
        timescale: u32,
        canonical_title_start: u64,
        max_rows: usize,
    ) -> Result<Self, RangeError> {
        if binding.source_generation.is_empty() || binding.contract.is_empty() || timescale == 0 {
            return Err(RangeError::InvalidBinding);
        }
        if max_rows == 0 {
            return Err(RangeError::RowLimit);
        }
        Ok(Self {
            binding,
            timescale,
            title_start: canonical_title_start,
            max_rows,
            rows: Vec::new(),
            observed_beginning: false,
            eof_end: None,
        })
    }

    pub fn binding(&self) -> &RangeBinding {
        &self.binding
    }
    pub fn timescale(&self) -> u32 {
        self.timescale
    }
    pub fn rows(&self) -> &[IndexRow] {
        &self.rows
    }
    pub fn eof_end_ticks(&self) -> Option<u64> {
        self.eof_end
    }

    /// Merge one contiguous, ordered batch. Duplicate rows must match every
    /// canonical field; a partial interval overlap is always refused.
    pub fn insert(
        &mut self,
        binding: &RangeBinding,
        batch: &[IndexRow],
        boundaries: RangeBoundaries,
    ) -> Result<(), RangeError> {
        if binding != &self.binding {
            return Err(RangeError::BindingMismatch);
        }
        if batch.is_empty() {
            return Err(RangeError::EmptyBatch);
        }
        if batch.len() > self.max_rows {
            return Err(RangeError::RowLimit);
        }
        let first = &batch[0];
        if first.dts < self.title_start || boundaries.begins_title && first.dts != self.title_start
        {
            return Err(RangeError::ConflictingBoundary);
        }
        let mut end = row_end(first)?;
        for row in &batch[1..] {
            if row.dts != end {
                return Err(RangeError::InvalidRows);
            }
            end = row_end(row)?;
        }
        let eof_end = if boundaries.eof {
            if self.eof_end.is_some_and(|prior| prior != end) {
                return Err(RangeError::ConflictingBoundary);
            }
            Some(end)
        } else {
            self.eof_end
        };
        let mut merged = Vec::with_capacity(
            self.rows
                .len()
                .saturating_add(batch.len())
                .min(self.max_rows),
        );
        let (mut old, mut new) = (0, 0);
        while old < self.rows.len() || new < batch.len() {
            let row = match (self.rows.get(old), batch.get(new)) {
                (Some(a), Some(b)) if a.dts == b.dts => {
                    if a != b {
                        return Err(RangeError::ConflictingRows);
                    }
                    old += 1;
                    new += 1;
                    a
                }
                (Some(a), Some(b)) if a.dts < b.dts => {
                    old += 1;
                    a
                }
                (Some(_), Some(b)) => {
                    new += 1;
                    b
                }
                (Some(a), None) => {
                    old += 1;
                    a
                }
                (None, Some(b)) => {
                    new += 1;
                    b
                }
                (None, None) => break,
            };
            let row_end = row_end(row)?;
            if merged
                .last()
                .is_some_and(|prior: &IndexRow| prior.dts + prior.duration > row.dts)
            {
                return Err(RangeError::ConflictingRows);
            }
            if eof_end.is_some_and(|eof| row_end > eof) {
                return Err(RangeError::ConflictingBoundary);
            }
            if merged.len() == self.max_rows {
                return Err(RangeError::RowLimit);
            }
            merged.push(row.clone());
        }
        self.rows = merged;
        self.observed_beginning |= boundaries.begins_title;
        self.eof_end = eof_end;
        Ok(())
    }

    /// The earliest unobserved span, or None exactly when [`Self::is_complete`].
    pub fn first_gap(&self) -> Option<RangeGap> {
        let first = self.rows.first();
        if !self.observed_beginning || first.is_none_or(|row| row.dts != self.title_start) {
            return Some(RangeGap {
                start: None,
                end: first.map(|row| row.dts),
            });
        }
        for pair in self.rows.windows(2) {
            let end = pair[0].dts + pair[0].duration;
            if end != pair[1].dts {
                return Some(RangeGap {
                    start: Some(end),
                    end: Some(pair[1].dts),
                });
            }
        }
        let end = self.rows.last().map(|row| row.dts + row.duration);
        (end != self.eof_end).then_some(RangeGap {
            start: end,
            end: None,
        })
    }

    /// Complete only after explicit beginning/EOF observations and gap-free
    /// coverage from the caller's canonical title start through that EOF.
    pub fn is_complete(&self) -> bool {
        self.observed_beginning
            && self
                .rows
                .first()
                .is_some_and(|row| row.dts == self.title_start)
            && self
                .rows
                .last()
                .is_some_and(|row| Some(row.dts + row.duration) == self.eof_end)
            && self
                .rows
                .windows(2)
                .all(|pair| pair[0].dts + pair[0].duration == pair[1].dts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fmp4::CutClass;

    fn binding() -> RangeBinding {
        RangeBinding {
            source_generation: "source-1".into(),
            contract: "copy-1".into(),
        }
    }
    fn row(dts: u64) -> IndexRow {
        IndexRow {
            dts,
            duration: 10,
            bytes: 100,
            video_bytes: 80,
            class: CutClass::CleanIdr,
        }
    }
    fn time(dts: i64, num: u32, den: u32) -> SourceTimestamp {
        SourceTimestamp {
            dts,
            time_base_num: num,
            time_base_den: den,
        }
    }

    #[test]
    fn signed_decode_clocks_normalize_exactly_without_rounding() {
        assert_eq!(
            normalize_to_index_ticks(time(-90, 1, 90), time(-2, 1, 1), 7, 90),
            Ok(97)
        );
        assert_eq!(
            normalize_to_index_ticks(time(-90, 1, 90), time(-1, 1, 1), 7, 90),
            Ok(7)
        );
        assert_eq!(
            normalize_to_index_ticks(time(1, 1, 3), time(0, 1, 1), 0, 10),
            Err(RangeError::NonIntegralTime)
        );
        assert_eq!(
            normalize_to_index_ticks(time(0, 1, 1), time(1, 1, 1), 0, 1),
            Err(RangeError::NegativeTime)
        );
        assert_eq!(
            normalize_to_index_ticks(time(1, 1, 1), time(0, 1, 1), u64::MAX, 1),
            Err(RangeError::TimeOverflow)
        );
        assert_eq!(
            normalize_to_index_ticks(time(1, 1, 0), time(0, 1, 1), 0, 1),
            Err(RangeError::InvalidClock)
        );
    }

    #[test]
    fn equal_large_clocks_cancel_before_scale_multiplication() {
        let origin = time(i64::MIN, u32::MAX, u32::MAX);
        assert_eq!(
            normalize_to_index_ticks(origin, origin, 17, u32::MAX),
            Ok(17)
        );
        assert_eq!(
            normalize_to_index_ticks(time(1, 1, 6), time(1, 1, 10), 7, 15),
            Ok(8)
        );
    }

    #[test]
    fn eof_tail_and_beginning_are_not_complete_until_sparse_gap_is_filled() {
        let key = binding();
        let mut index =
            SparseIndexRows::new(key.clone(), 100, 7, 4).expect("valid sparse-range fixture");
        index
            .insert(
                &key,
                &[row(27)],
                RangeBoundaries {
                    eof: true,
                    ..Default::default()
                },
            )
            .expect("valid sparse-range fixture");
        assert!(!index.is_complete());
        index
            .insert(
                &key,
                &[row(7)],
                RangeBoundaries {
                    begins_title: true,
                    ..Default::default()
                },
            )
            .expect("valid sparse-range fixture");
        assert!(!index.is_complete());
        index
            .insert(&key, &[row(17), row(27)], RangeBoundaries::default())
            .expect("valid sparse-range fixture");
        assert!(index.is_complete());
        assert_eq!(index.rows(), &[row(7), row(17), row(27)]);
        assert_eq!(index.eof_end_ticks(), Some(37));
    }

    #[test]
    fn first_gap_walks_beginning_then_interior_then_eof_and_ends_with_completeness() {
        let key = binding();
        let mut index =
            SparseIndexRows::new(key.clone(), 100, 7, 8).expect("valid sparse-range fixture");
        assert_eq!(
            index.first_gap(),
            Some(RangeGap {
                start: None,
                end: None
            })
        );
        index
            .insert(&key, &[row(27)], RangeBoundaries::default())
            .expect("valid sparse-range fixture");
        assert_eq!(
            index.first_gap(),
            Some(RangeGap {
                start: None,
                end: Some(27)
            })
        );
        index
            .insert(
                &key,
                &[row(7)],
                RangeBoundaries {
                    begins_title: true,
                    ..Default::default()
                },
            )
            .expect("valid sparse-range fixture");
        assert_eq!(
            index.first_gap(),
            Some(RangeGap {
                start: Some(17),
                end: Some(27)
            })
        );
        index
            .insert(&key, &[row(17), row(27)], RangeBoundaries::default())
            .expect("valid sparse-range fixture");
        assert_eq!(
            index.first_gap(),
            Some(RangeGap {
                start: Some(37),
                end: None
            })
        );
        assert!(!index.is_complete());
        index
            .insert(
                &key,
                &[row(37)],
                RangeBoundaries {
                    eof: true,
                    ..Default::default()
                },
            )
            .expect("valid sparse-range fixture");
        assert_eq!(index.first_gap(), None);
        assert!(index.is_complete());
    }

    #[test]
    fn conflicts_and_resource_limits_leave_prior_rows_unchanged() {
        let key = binding();
        let mut index =
            SparseIndexRows::new(key.clone(), 100, 7, 2).expect("valid sparse-range fixture");
        index
            .insert(&key, &[row(7)], RangeBoundaries::default())
            .expect("valid sparse-range fixture");
        for changed in [
            IndexRow {
                bytes: 101,
                ..row(7)
            },
            IndexRow {
                video_bytes: 81,
                ..row(7)
            },
            IndexRow {
                duration: 11,
                ..row(7)
            },
            IndexRow {
                class: CutClass::CleanCra,
                ..row(7)
            },
        ] {
            assert_eq!(
                index.insert(&key, &[changed], RangeBoundaries::default()),
                Err(RangeError::ConflictingRows)
            );
        }
        assert_eq!(
            index.insert(&key, &[row(12)], RangeBoundaries::default()),
            Err(RangeError::ConflictingRows)
        );
        assert_eq!(
            index.insert(&key, &[row(17), row(27)], RangeBoundaries::default()),
            Err(RangeError::RowLimit)
        );
        let mut foreign = key.clone();
        foreign.contract = "other".into();
        assert_eq!(
            index.insert(&foreign, &[row(17)], RangeBoundaries::default()),
            Err(RangeError::BindingMismatch)
        );
        foreign = key.clone();
        foreign.source_generation = "replaced".into();
        assert_eq!(
            index.insert(&foreign, &[row(17)], RangeBoundaries::default()),
            Err(RangeError::BindingMismatch)
        );
        assert_eq!(index.rows(), &[row(7)]);
    }

    #[test]
    fn invalid_order_duration_and_physical_boundaries_are_rejected() {
        let key = binding();
        let mut index =
            SparseIndexRows::new(key.clone(), 100, 7, 8).expect("valid sparse-range fixture");
        assert_eq!(
            index.insert(&key, &[], RangeBoundaries::default()),
            Err(RangeError::EmptyBatch)
        );
        assert_eq!(
            index.insert(&key, &[row(u64::MAX)], RangeBoundaries::default()),
            Err(RangeError::InvalidRows)
        );
        let mut zero = row(7);
        zero.duration = 0;
        assert_eq!(
            index.insert(&key, &[zero], RangeBoundaries::default()),
            Err(RangeError::InvalidRows)
        );
        assert_eq!(
            index.insert(&key, &[row(17), row(7)], RangeBoundaries::default()),
            Err(RangeError::InvalidRows)
        );
        assert_eq!(
            index.insert(&key, &[row(7), row(27)], RangeBoundaries::default()),
            Err(RangeError::InvalidRows)
        );
        assert_eq!(
            index.insert(
                &key,
                &[row(17)],
                RangeBoundaries {
                    begins_title: true,
                    eof: false
                }
            ),
            Err(RangeError::ConflictingBoundary)
        );
        index
            .insert(
                &key,
                &[row(7)],
                RangeBoundaries {
                    begins_title: true,
                    eof: true,
                },
            )
            .expect("valid sparse-range fixture");
        assert!(index.is_complete());
        assert_eq!(
            index.insert(&key, &[row(17)], RangeBoundaries::default()),
            Err(RangeError::ConflictingBoundary)
        );
        assert_eq!(
            index.insert(
                &key,
                &[row(7), row(17)],
                RangeBoundaries {
                    eof: true,
                    ..Default::default()
                }
            ),
            Err(RangeError::ConflictingBoundary)
        );
        assert!(index.is_complete());
    }
}
