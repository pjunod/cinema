//! Shared provider pacing is attached to maintenance clients explicitly.
//! Waiting holds no publication lock, and network/sleep cancellation is cheap:
//! these futures own no subprocess that needs to be reaped.
use super::Provider;
use crate::{
    error::{MetadataError, StoreError},
    store::background_jobs_provider::{ProviderBudgetAction, ProviderBudgetOutcome},
};
use std::{sync::Arc, time::Duration};

#[async_trait::async_trait]
pub trait ProviderBudget: Send + Sync {
    async fn update(
        &self,
        provider: Provider,
        action: ProviderBudgetAction,
    ) -> Result<ProviderBudgetOutcome, StoreError>;
}
pub type SharedProviderBudget = Option<Arc<dyn ProviderBudget>>;

pub(crate) async fn cancellable<T>(
    operation: impl std::future::Future<Output = T>,
) -> Result<T, MetadataError> {
    if let Some(cancel) = crate::process::bounded::cancellation() {
        tokio::select! {
            biased;
            () = cancel.cancelled() => Err(MetadataError::Http("provider work cancelled".into())),
            output = operation => Ok(output),
        }
    } else {
        Ok(operation.await)
    }
}

pub(crate) async fn acquire(
    budget: &SharedProviderBudget,
    provider: Provider,
) -> Result<(), MetadataError> {
    let Some(budget) = budget else {
        return Ok(());
    };
    let started = tokio::time::Instant::now();
    loop {
        let outcome = cancellable(budget.update(provider, ProviderBudgetAction::Charge))
            .await?
            .map_err(|error| MetadataError::Http(format!("provider allowance: {error}")))?;
        match outcome {
            ProviderBudgetOutcome::Charged => return Ok(()),
            ProviderBudgetOutcome::Wait { until_ms } => {
                let now = crate::cluster::coordination::unix_ms()
                    .map_err(|error| MetadataError::Http(error.to_string()))?;
                let wait =
                    Duration::from_millis(until_ms.saturating_sub(now).clamp(1, 5_000) as u64);
                if started.elapsed().saturating_add(wait) >= super::PROVIDER_CALL_BUDGET {
                    return Err(MetadataError::Timeout(
                        "shared provider cooldown exceeds this call's budget".into(),
                    ));
                }
                cancellable(tokio::time::sleep(wait)).await?;
            }
            _ => return Err(MetadataError::Http("provider ownership lost".into())),
        }
    }
}

pub(crate) async fn observe(
    budget: &SharedProviderBudget,
    provider: Provider,
    response: &reqwest::Response,
) -> Result<(), MetadataError> {
    let Some(budget) = budget else {
        return Ok(());
    };
    let seconds = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<i64>().ok())
            .filter(|value| *value >= 0)
    };
    let rate = seconds("x-ratelimit-limit").filter(|limit| *limit > 0);
    let interval_ms =
        rate.map(|limit| (60_000_i64.div_euclid(limit).saturating_add(1)).clamp(100, 60_000));
    let now = crate::cluster::coordination::unix_ms()
        .map_err(|error| MetadataError::Http(error.to_string()))?;
    let retry_ms = seconds("retry-after").unwrap_or(0).saturating_mul(1000);
    let reset_ms = if seconds("x-ratelimit-remaining") == Some(0) {
        seconds("x-ratelimit-reset")
            .map(|reset| reset.saturating_mul(1000).saturating_sub(now))
            .unwrap_or(60_000)
    } else {
        0
    };
    let cooldown_ms = retry_ms
        .max(reset_ms)
        .max(if response.status().as_u16() == 429 && retry_ms == 0 {
            match provider {
                Provider::Tmdb => 1_000,
                Provider::AniList => 60_000,
            }
        } else {
            0
        })
        .clamp(0, 86_400_000);
    if cooldown_ms == 0 && interval_ms.is_none() {
        return Ok(());
    }
    match cancellable(budget.update(
        provider,
        ProviderBudgetAction::Observe {
            cooldown_ms,
            interval_ms,
        },
    ))
    .await?
    .map_err(|error| MetadataError::Http(format!("provider allowance: {error}")))?
    {
        ProviderBudgetOutcome::Observed => Ok(()),
        _ => Err(MetadataError::Http("provider ownership lost".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Refused(AtomicUsize);
    #[async_trait::async_trait]
    impl ProviderBudget for Refused {
        async fn update(
            &self,
            _: Provider,
            _: ProviderBudgetAction,
        ) -> Result<ProviderBudgetOutcome, StoreError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(ProviderBudgetOutcome::LostAuthority)
        }
    }

    #[tokio::test]
    async fn stale_maintenance_owner_never_reaches_the_provider_transport() {
        let budget = Arc::new(Refused(AtomicUsize::new(0)));
        let client = crate::metadata::AniListClient::new()
            .with_base("http://127.0.0.1:1")
            .with_budget(Some(budget.clone()));
        let error = client
            .find_anime("title")
            .await
            .expect_err("owner refused before network");
        assert!(
            error.to_string().contains("provider ownership lost"),
            "{error}"
        );
        assert_eq!(budget.0.load(Ordering::SeqCst), 1);
        let cancel = tokio_util::sync::CancellationToken::new();
        cancel.cancel();
        let error = crate::process::bounded::cancellable(cancel, client.find_anime("cancelled"))
            .await
            .expect_err("cancelled before allowance or network");
        assert!(
            error.to_string().contains("provider work cancelled"),
            "{error}"
        );
        assert_eq!(budget.0.load(Ordering::SeqCst), 1);
    }
}
