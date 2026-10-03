//! Owned Source producer admission and lifetime. HTTP callers only wait.
use super::*;

impl TranscodeManager {
    /// The same actual admission/materialization allowance used by the Source
    /// actor and its peer start handler. Transport adds its separate margin.
    /// A response deadline never certifies producer or writer settlement.
    #[allow(dead_code)] // The qualified peer start consumer is being integrated.
    pub(crate) async fn source_worker_start_budget(
        &self,
        prepared: &crate::http::hls::PreparedSourcePlayback,
    ) -> Result<Duration, String> {
        self.source_start_budget_for_request(prepared.request())
            .await
    }

    async fn source_start_budget_for_request(
        &self,
        request: &SessionRequest,
    ) -> Result<Duration, String> {
        let settings = self.vod_settings(request).await?.ok_or_else(|| {
            vod_refusal_error(
                "vod_disabled",
                "VOD presentation is disabled for maintenance",
            )
        })?;
        Ok(crate::admission::QUEUE_WAIT + settings.materialize_budget)
    }
}

#[cfg(test)]
#[path = "tests/source_actor.rs"]
mod tests;
