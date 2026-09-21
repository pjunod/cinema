use std::collections::BTreeMap;
use std::sync::Arc;

use super::{
    inspection_to_store, OpticalDriveManager, OpticalDriveSnapshot, OpticalHostAdapter,
    OpticalHostError, OpticalLifecycleError, OpticalReadPermit,
};
use crate::config::OpticalDriveConfig;
use crate::error::StoreError;
use crate::store::OpticalStore;

#[derive(Debug, thiserror::Error)]
pub enum OpticalServiceError {
    #[error("unknown optical drive")]
    UnknownDrive,
    #[error(transparent)]
    Lifecycle(#[from] OpticalLifecycleError),
    #[error(transparent)]
    Host(#[from] OpticalHostError),
    #[error("optical inspection failed validation: {0}")]
    Inspection(String),
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Coordinates trusted node-local paths with the path-free public lifecycle.
///
/// Runtime enablement owns whether a service is actively observed; this type
/// has no readiness gate. If an operator enables optical with an unmet helper
/// or device requirement, the requested operation returns that concrete host
/// error and the daemon remains healthy.
pub struct OpticalService<S, H> {
    manager: OpticalDriveManager,
    drives: BTreeMap<String, OpticalDriveConfig>,
    store: Arc<S>,
    host: Arc<H>,
}

impl<S, H> OpticalService<S, H>
where
    S: OpticalStore,
    H: OpticalHostAdapter,
{
    pub fn new(
        owner_node_id: impl Into<String>,
        drives: Vec<OpticalDriveConfig>,
        store: Arc<S>,
        host: Arc<H>,
    ) -> Self {
        let manager = OpticalDriveManager::new(owner_node_id, &drives);
        let drives = drives
            .into_iter()
            .map(|drive| (drive.id.clone(), drive))
            .collect();
        Self {
            manager,
            drives,
            store,
            host,
        }
    }

    pub fn manager(&self) -> &OpticalDriveManager {
        &self.manager
    }

    pub fn requirements(
        &self,
        drive_id: &str,
    ) -> Result<Vec<super::HostRequirement>, OpticalServiceError> {
        let drive = self
            .drives
            .get(drive_id)
            .ok_or(OpticalServiceError::UnknownDrive)?;
        Ok(self.host.requirements(drive))
    }

    /// Handle one insertion/change edge through inspection and durable
    /// publication. A polling loop must call this only on an observed edge;
    /// an uncertain-change event is itself an edge and deliberately re-fences.
    pub async fn inspect_insertion(
        &self,
        drive_id: &str,
        now_ms: i64,
    ) -> Result<OpticalDriveSnapshot, OpticalServiceError> {
        let drive = self
            .drives
            .get(drive_id)
            .ok_or(OpticalServiceError::UnknownDrive)?;
        let generation = self.manager.observe_insertion(drive_id)?;
        let permit = self.manager.claim_inspection(drive_id, &generation)?;
        let response = match self.host.inspect(drive, &generation).await {
            Ok(response) => response,
            Err(error) => {
                let _ = permit.publish_failed(&error.to_string());
                return Err(error.into());
            }
        };
        let inspection = match inspection_to_store(&response, now_ms) {
            Ok(inspection) => inspection,
            Err(error) => {
                let detail = error.to_string();
                let _ = permit.publish_failed(&detail);
                return Err(OpticalServiceError::Inspection(detail));
            }
        };
        self.store.upsert_optical_inspection(&inspection).await?;
        permit.publish_ready(&inspection.disc.disc_id)?;
        self.manager
            .snapshot(drive_id)
            .ok_or(OpticalServiceError::UnknownDrive)
    }

    pub fn remove(&self, drive_id: &str) -> Result<(), OpticalServiceError> {
        self.manager.observe_removal(drive_id)?;
        Ok(())
    }

    pub fn claim_playback(
        &self,
        drive_id: &str,
        expected_generation: &str,
        expected_disc_id: &str,
        title_id: &str,
        session_id: &str,
    ) -> Result<OpticalReadPermit, OpticalServiceError> {
        Ok(self.manager.claim_playback(
            drive_id,
            expected_generation,
            expected_disc_id,
            title_id,
            session_id,
        )?)
    }

    /// Execute a previously authenticated/authorized eject against the exact
    /// insertion and, when busy, exact stopped session.
    pub async fn eject(
        &self,
        drive_id: &str,
        expected_generation: &str,
        stopped_session_id: Option<&str>,
    ) -> Result<(), OpticalServiceError> {
        let drive = self
            .drives
            .get(drive_id)
            .ok_or(OpticalServiceError::UnknownDrive)?;
        self.manager.authorize_eject(
            drive_id,
            expected_generation,
            stopped_session_id,
        )?;
        self.host.eject(drive).await?;
        self.manager.observe_removal(drive_id)?;
        Ok(())
    }
}
