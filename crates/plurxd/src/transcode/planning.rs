use super::*;

/// Give back what observation spent, so a bounded probe cannot shorten the
/// encode it was meant to inform.
pub(super) fn retain_production_budget_after_planning(
    production_deadline: Instant,
    observation_duration: Duration,
) -> Instant {
    production_deadline
        .checked_add(observation_duration)
        .unwrap_or(production_deadline)
}

#[derive(Clone, Copy)]
pub(super) enum BoundPlanCaller {
    Pretranscode,
    Vod,
}

impl BoundPlanCaller {
    /// The class and purpose of the decode-fact probes this caller waits on.
    /// A VOD start waits up to `DECODE_PLAN_PROBE_BUDGET` with a viewer in
    /// front of it; the pre-transcode pass has nobody waiting.
    pub(super) const fn decode_fact_work(self) -> crate::process_control::ChildWork {
        match self {
            Self::Pretranscode => crate::process_control::ChildWork::background(
                "decode-fact probe for the pre-transcode pass",
            ),
            Self::Vod => {
                crate::process_control::ChildWork::realtime("decode-fact probe for a session start")
            }
        }
    }

    pub(super) fn decode_fact_source(
        self,
        handle: Arc<std::fs::File>,
        offset_gate: Arc<tokio::sync::Semaphore>,
    ) -> crate::decode_facts::DecodeFactSource {
        crate::decode_facts::DecodeFactSource::new(handle, offset_gate, self.decode_fact_work())
    }

    pub(super) fn finish(
        self,
        result: Result<ResolvedTranscode, String>,
    ) -> Result<ResolvedTranscode, String> {
        match self {
            Self::Pretranscode => result,
            Self::Vod => {
                result.map_err(|error| vod_refusal_error("vod_decoder_plan_refused", error))
            }
        }
    }
}

pub(super) fn tone_map_pref() -> ToneMap {
    match std::env::var("PLURX_TONEMAP").as_deref() {
        Ok("libplacebo") => ToneMap::Libplacebo,
        Ok("off" | "none" | "passthrough") => ToneMap::None,
        _ => ToneMap::Zscale,
    }
}

#[cfg(test)]
pub(crate) async fn unverified_hevc_copy_enabled(store: &dyn Store) -> Result<bool, String> {
    let value = store
        .get_setting(plurx_core::store::keys::HEVC_UNVERIFIED_COPY)
        .await
        .map_err(|error| format!("reading HEVC copy preference: {error}"))?;
    Ok(plurx_core::store::stored_switch(value.as_deref(), false))
}
