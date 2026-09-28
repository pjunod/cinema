//! Runtime-controlled observation of node-local optical drives.
//!
//! The Store switch is authoritative. Readiness remains advisory: enabling an
//! unavailable helper does not rewrite the choice, it simply leaves concrete
//! host errors for diagnostics and operations to report.

use std::time::{SystemTime, UNIX_EPOCH};

use plurx_core::store::{keys, stored_switch};

use crate::state::AppState;

const OPTICAL_READER_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Close admission, durably terminate every active optical session, and wait
/// until each exact physical-reader permit has completed cleanup. The saved
/// runtime choice remains authoritative; this is the operational drain owed
/// by a disable or process shutdown, not a readiness gate.
pub(crate) async fn deactivate_and_drain(state: &AppState, reason: &'static str) -> bool {
    let session_ids = state.optical.deactivate();
    for session_id in session_ids {
        let status = crate::http::hls::release_with_terminal(
            state.clone(),
            session_id.clone(),
            crate::vodserve::Terminal::Revoked,
            reason,
        )
        .await;
        if !status.is_success() && status != axum::http::StatusCode::NOT_FOUND {
            tracing::warn!(%status, "optical durable session drain was not confirmed");
        }
        let _ = state.transcode.stop_session(&session_id, reason).await;
    }

    tokio::time::timeout(OPTICAL_READER_DRAIN_TIMEOUT, async {
        while state.optical.manager().active_reader_count() != 0 {
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .is_ok()
}

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
        } else if was_enabled
            && !deactivate_and_drain(&state, "optical source revoked by runtime disable").await
        {
            tracing::warn!("optical disable timed out waiting for physical readers");
        }
        was_enabled = enabled;

        tokio::select! {
            () = shutdown.cancelled() => {
                if !deactivate_and_drain(&state, "optical source revoked by server shutdown").await {
                    tracing::warn!("optical shutdown timed out waiting for physical readers");
                }
                return;
            }
            () = tokio::time::sleep(state.optical.poll_interval()) => {}
        }
    }
}
