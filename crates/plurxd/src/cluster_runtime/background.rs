//! Replication workers are composed only for an explicitly clustered runtime.
use crate::state::AppState;
use plurx_core::storage::mode::StorageMode;
use tokio_util::sync::CancellationToken;

pub(crate) fn spawn_background_loops(state: &AppState, background_shutdown: &CancellationToken) {
    if state.storage_mode != StorageMode::Cluster {
        return;
    }

    tokio::spawn(crate::clock_offset::run(
        state.clone(),
        background_shutdown.clone(),
    ));

    tokio::spawn(
        crate::http::cluster_operations::membership_status_cache_loop(
            state.clone(),
            background_shutdown.clone(),
        ),
    );
    tokio::spawn(
        crate::http::internal_auth_revocation::cache_admin_revocation_activation_loop(
            state.clone(),
            background_shutdown.clone(),
        ),
    );
    tokio::spawn(crate::http::cluster_operations::peer_status_cache_loop(
        state.clone(),
        background_shutdown.clone(),
    ));
    let replication = state.replication.clone();
    tokio::spawn(replication.passive_metrics_loop(background_shutdown.clone().cancelled_owned()));
    tokio::spawn(
        state
            .serving
            .clone()
            .monitor_loop(background_shutdown.clone()),
    );
    state.membership.register_media_relay_acquisition();
    tokio::spawn(state.membership.clone().heartbeat_loop());

    tokio::spawn(crate::media_sessions::takeover_loop(state.clone()));

    tokio::spawn(state.membership.clone().offline_source_probe_loop());
}
