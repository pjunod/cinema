//! The optional Hiqlite watch adapter for shared settings observation.
use crate::settings_changes::{SettingsChangeObserver, SettingsChangeSource};

#[derive(Clone)]
struct HiqliteSettingsObserver {
    metrics: hiqlite::LocalDbRaftMetrics,
}

#[async_trait::async_trait]
impl SettingsChangeObserver for HiqliteSettingsObserver {
    fn clone_observer(&self) -> Box<dyn SettingsChangeObserver> {
        // LocalDbRaftMetrics clones the watch receiver, preserving a separate
        // seen-version cursor for every settings reader.
        Box::new(self.clone())
    }
    fn applied_index(&self) -> Option<u64> {
        self.metrics.snapshot().last_applied_index
    }
    fn live(&self) -> bool {
        self.metrics.membership_snapshot().running
    }
    async fn changed(&mut self) -> bool {
        self.metrics.wait_for_change().await
    }
}

pub(super) fn from_client(client: &hiqlite::Client) -> SettingsChangeSource {
    match client.local_db_raft_metrics() {
        Ok(metrics) => SettingsChangeSource::Applied(Box::new(HiqliteSettingsObserver { metrics })),
        Err(_) => SettingsChangeSource::Polled,
    }
}

pub(super) fn from_metrics(metrics: hiqlite::LocalDbRaftMetrics) -> SettingsChangeSource {
    SettingsChangeSource::Applied(Box::new(HiqliteSettingsObserver { metrics }))
}
