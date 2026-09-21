//! Runtime-controlled observation of node-local optical drives.
//!
//! The Store switch is authoritative. Readiness remains advisory: enabling an
//! unavailable helper does not rewrite the choice, it simply leaves concrete
//! host errors for diagnostics and operations to report.

use std::time::{SystemTime, UNIX_EPOCH};

use plurx_core::store::{keys, stored_switch};

use crate::state::AppState;

pub(crate) async fn observation_loop(
    state: AppState,
    shutdown: tokio_util::sync::CancellationToken,
) {
    if state.optical.configured_drive_ids().next().is_none() {
        shutdown.cancelled().await;
        return;
    }

    let mut was_enabled = false;
    loop {
        let enabled = match state.store.get_setting(keys::OPTICAL_ENABLED).await {
            Ok(value) => stored_switch(value.as_deref(), false),
            Err(error) => {
                tracing::warn!(%error, "could not read optical runtime setting");
                false
            }
        };

        if enabled {
            let now_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .and_then(|duration| i64::try_from(duration.as_millis()).ok())
                .unwrap_or(0);
            for (drive_id, error) in state.optical.observe_once(now_ms).await {
                tracing::debug!(%drive_id, %error, "optical observation did not complete");
            }
        } else if was_enabled {
            state.optical.deactivate();
        }
        was_enabled = enabled;

        tokio::select! {
            () = shutdown.cancelled() => {
                state.optical.deactivate();
                return;
            }
            () = tokio::time::sleep(state.optical.poll_interval()) => {}
        }
    }
}
