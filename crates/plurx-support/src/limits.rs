//! Operator may lower resource budgets; fixed format and security ceilings remain.
use crate::store::{Fault, Result};
use serde::{Deserialize, Serialize};
use std::{io::Read, path::Path};

#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Limits {
    pub uploads_per_enrollment: u32,
    pub uploads_per_publisher: u32,
    pub submissions_per_enrollment_day: u32,
    pub submissions_per_publisher_day: u32,
    pub bytes_per_publisher: u64,
    pub bytes_global: u64,
    pub concurrent_requests: u32,
    pub concurrent_inspections: u32,
    pub code_attempts_per_minute: u32,
    pub publishers: u32,
    pub report_rows: u32,
    pub capability_rows: u32,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            uploads_per_enrollment: 2,
            uploads_per_publisher: 5,
            submissions_per_enrollment_day: 10,
            submissions_per_publisher_day: 100,
            bytes_per_publisher: 1024 * 1024 * 1024,
            bytes_global: 20 * 1024 * 1024 * 1024,
            concurrent_requests: 8,
            concurrent_inspections: 2,
            code_attempts_per_minute: 5,
            publishers: 1000,
            report_rows: 50000,
            capability_rows: 50000,
        }
    }
}
impl Limits {
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let value = if let Some(path) = path {
            let mut bytes = Vec::new();
            std::fs::File::open(path)?
                .take(16385)
                .read_to_end(&mut bytes)?;
            serde_json::from_value(
                plurx_core::diagnostics::strict_json(&bytes, 16384).map_err(|_| Fault::Invalid)?,
            )
            .map_err(|_| Fault::Invalid)?
        } else {
            Self::default()
        };
        let ceiling = Self::default();
        for (n, max) in [
            (value.uploads_per_enrollment, ceiling.uploads_per_enrollment),
            (value.uploads_per_publisher, ceiling.uploads_per_publisher),
            (
                value.submissions_per_enrollment_day,
                ceiling.submissions_per_enrollment_day,
            ),
            (
                value.submissions_per_publisher_day,
                ceiling.submissions_per_publisher_day,
            ),
            (value.concurrent_requests, ceiling.concurrent_requests),
            (value.concurrent_inspections, ceiling.concurrent_inspections),
            (
                value.code_attempts_per_minute,
                ceiling.code_attempts_per_minute,
            ),
            (value.publishers, ceiling.publishers),
            (value.report_rows, ceiling.report_rows),
            (value.capability_rows, ceiling.capability_rows),
        ] {
            if n == 0 || n > max {
                return Err(Fault::Invalid);
            }
        }
        if !(16 * 1024 * 1024..=ceiling.bytes_per_publisher).contains(&value.bytes_per_publisher)
            || !(16 * 1024 * 1024..=ceiling.bytes_global).contains(&value.bytes_global)
            || value.bytes_per_publisher > value.bytes_global
        {
            return Err(Fault::Invalid);
        }
        Ok(value)
    }
}
