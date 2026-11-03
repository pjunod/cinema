//! Scalar native-parser source service in the existing seccomp owner.
//! All descriptors are charged, including parser/tool reads: this deliberately
//! avoids mutable FD-alias classification and its CONTINUE TOCTOU boundary.
use super::DecodeFactError;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub(super) const MAX_READ: usize = 1024 * 1024;

pub(super) struct ReadPermission {
    background: Option<Arc<crate::background_jobs::SourceReadBudget>>,
    foreground: Option<Arc<crate::source_custody::SourceChildReadBudget>>,
    runtime: tokio::runtime::Handle,
}
impl ReadPermission {
    pub(super) async fn for_source(
        source: &std::fs::File,
    ) -> Result<Option<Arc<Self>>, DecodeFactError> {
        let background = crate::background_jobs::source_budget();
        let foreground = if background.is_none() {
            match crate::source_custody::current_request() {
                Some(request) => Some(
                    request
                        .register_child_reads(source, "strict-native-source-probe")
                        .await
                        .map_err(DecodeFactError::Read)?,
                ),
                None => None,
            }
        } else {
            None
        };
        if background.is_none() && foreground.is_none() {
            return Ok(None);
        }
        Ok(Some(Arc::new(Self {
            background,
            foreground,
            runtime: tokio::runtime::Handle::current(),
        })))
    }

    /// Reserve the complete immutable scalar count before the kernel read.
    /// Partial grants are accumulated without issuing partial/unbudgeted IO;
    /// EOF, failed, interrupted and short reads never manufacture refunds.
    pub(super) fn reserve(
        &self,
        count: usize,
        cancel: &CancellationToken,
    ) -> Result<(), DecodeFactError> {
        if count > MAX_READ {
            return Err(DecodeFactError::SourceService(
                "strict scalar source read exceeds one MiB service granularity".into(),
            ));
        }
        if count == 0 {
            return Ok(());
        }
        if cancel.is_cancelled() {
            return Err(DecodeFactError::Cancelled);
        }
        if let Some(budget) = &self.background {
            let mut left = count;
            while left > 0 {
                let issued = budget.allowance_blocking(left, cancel).map_err(|_| {
                    if cancel.is_cancelled() || budget.cancellation().is_cancelled() {
                        DecodeFactError::Cancelled
                    } else {
                        DecodeFactError::SourceService(
                            "background source read service unavailable".into(),
                        )
                    }
                })?;
                if issued == 0 || issued > left {
                    return Err(DecodeFactError::SourceService(
                        "invalid source service allowance".into(),
                    ));
                }
                left -= issued;
            }
            Ok(())
        } else {
            let foreground = self.foreground.as_ref().ok_or_else(|| {
                DecodeFactError::SourceService("source read permission unavailable".into())
            })?;
            self.runtime
                .block_on(foreground.reserve_bytes(count, cancel))
                .map_err(|_| {
                    if cancel.is_cancelled() {
                        DecodeFactError::Cancelled
                    } else {
                        DecodeFactError::SourceService(
                            "foreground source read service unavailable".into(),
                        )
                    }
                })
        }
    }
}

tokio::task_local! { static PERMISSION: Option<Arc<ReadPermission>>; }
pub(super) fn current() -> Option<Arc<ReadPermission>> {
    PERMISSION.try_with(Clone::clone).ok().flatten()
}
pub(super) async fn scope<T>(
    permission: Option<Arc<ReadPermission>>,
    work: impl std::future::Future<Output = T>,
) -> T {
    PERMISSION.scope(permission, work).await
}
