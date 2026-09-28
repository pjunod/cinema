use std::collections::BTreeMap;
use std::sync::Arc;

use super::{
    inspection_to_store, OpticalDriveManager, OpticalDriveSnapshot, OpticalHostAdapter,
    OpticalHostError, OpticalLifecycleError, OpticalMediaPresence, OpticalPlaybackClaim,
    OpticalReadPermit,
};
use super::{OpticalTitle, PlaybackSourceRef, ResolvedInput};
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
pub struct OpticalService<S: ?Sized, H> {
    manager: OpticalDriveManager,
    drives: BTreeMap<String, OpticalDriveConfig>,
    store: Arc<S>,
    host: Arc<H>,
    poll_interval: std::time::Duration,
}

/// Full capability handed to the playback controller. Dropping it releases
/// the exclusive physical reader and returns the drive to ready.
pub struct OpticalPlaybackLease {
    pub source: PlaybackSourceRef,
    pub input: ResolvedInput,
    pub permit: OpticalReadPermit,
}

impl OpticalPlaybackLease {
    pub fn is_current(&self) -> bool {
        self.permit.is_current()
    }
}

pub enum OpticalTitleClaim {
    Claimed(OpticalPlaybackLease),
    Replay { session_id: String },
}

impl<S, H> OpticalService<S, H>
where
    S: OpticalStore + ?Sized,
    H: OpticalHostAdapter,
{
    pub fn new(
        owner_node_id: impl Into<String>,
        drives: Vec<OpticalDriveConfig>,
        store: Arc<S>,
        host: Arc<H>,
        poll_interval: std::time::Duration,
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
            poll_interval,
        }
    }

    pub fn poll_interval(&self) -> std::time::Duration {
        self.poll_interval
    }

    pub fn configured_drive_ids(&self) -> impl Iterator<Item = &str> {
        self.drives.keys().map(String::as_str)
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
        let generation = self.manager.observe_insertion(drive_id)?;
        self.inspect_generation(drive_id, &generation, now_ms).await
    }

    /// Finish an already-minted insertion after a revoked reader has drained.
    /// This does not mint another generation, so clients keep observing one
    /// stable replacement epoch while the drive becomes available.
    async fn inspect_generation(
        &self,
        drive_id: &str,
        generation: &str,
        now_ms: i64,
    ) -> Result<OpticalDriveSnapshot, OpticalServiceError> {
        let drive = self
            .drives
            .get(drive_id)
            .ok_or(OpticalServiceError::UnknownDrive)?;
        let permit = self.manager.claim_inspection(drive_id, generation)?;
        let response = match self.host.inspect(drive, generation).await {
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

    /// Perform one cheap observation pass. Full inspection occurs only on the
    /// empty-to-present edge; stable present media is not repeatedly opened.
    pub async fn observe_once(&self, now_ms: i64) -> Vec<(String, OpticalServiceError)> {
        let mut errors = Vec::new();
        for (drive_id, drive) in &self.drives {
            let presence = match self.host.presence(drive).await {
                Ok(presence) => presence,
                Err(error) => {
                    errors.push((drive_id.clone(), error.into()));
                    continue;
                }
            };
            let state = self
                .manager
                .snapshot(drive_id)
                .map(|snapshot| snapshot.state);
            match (presence, state) {
                (
                    OpticalMediaPresence::Present | OpticalMediaPresence::Changed,
                    Some(super::OpticalDriveState::Empty),
                ) => {
                    if let Err(error) = self.inspect_insertion(drive_id, now_ms).await {
                        errors.push((drive_id.clone(), error));
                    }
                }
                (OpticalMediaPresence::Changed, Some(_)) => {
                    // A tray can be swapped completely between two polls.
                    // The kernel change edge therefore revokes the old
                    // generation even though no Empty state was observed.
                    if let Err(error) = self.inspect_insertion(drive_id, now_ms).await {
                        errors.push((drive_id.clone(), error));
                    }
                }
                (
                    OpticalMediaPresence::Present,
                    Some(super::OpticalDriveState::Inspecting { media_generation }),
                ) => {
                    if let Err(error) = self
                        .inspect_generation(drive_id, &media_generation, now_ms)
                        .await
                    {
                        errors.push((drive_id.clone(), error));
                    }
                }
                (OpticalMediaPresence::Empty, Some(super::OpticalDriveState::Empty))
                | (OpticalMediaPresence::Unknown, _) => {}
                (OpticalMediaPresence::Empty, Some(_)) => {
                    if let Err(error) = self.remove(drive_id) {
                        errors.push((drive_id.clone(), error));
                    }
                }
                // A stable present signal does not prove a change. Host event
                // adapters and Linux's media-changed ioctl provide explicit
                // change edges.
                (OpticalMediaPresence::Present, Some(_)) => {}
                (_, None) => errors.push((drive_id.clone(), OpticalServiceError::UnknownDrive)),
            }
        }
        errors
    }

    /// Disabling observation immediately fences all insertion generations.
    pub fn deactivate(&self) {
        for drive_id in self.drives.keys() {
            let _ = self.manager.observe_removal(drive_id);
        }
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

    pub fn claim_playback_title(
        &self,
        drive_id: &str,
        expected_generation: &str,
        expected_disc_id: &str,
        title: &OpticalTitle,
        angle: u32,
        session_id: &str,
    ) -> Result<OpticalPlaybackLease, OpticalServiceError> {
        match self.claim_playback_title_request(
            drive_id,
            expected_generation,
            expected_disc_id,
            title,
            angle,
            session_id,
            session_id,
            session_id,
        )? {
            OpticalTitleClaim::Claimed(lease) => Ok(lease),
            OpticalTitleClaim::Replay { .. } => Err(OpticalLifecycleError::Busy.into()),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn claim_playback_title_request(
        &self,
        drive_id: &str,
        expected_generation: &str,
        expected_disc_id: &str,
        title: &OpticalTitle,
        angle: u32,
        session_id: &str,
        request_id: &str,
        request_digest: &str,
    ) -> Result<OpticalTitleClaim, OpticalServiceError> {
        if title.disc_id != expected_disc_id || angle == 0 || angle > title.angles {
            return Err(OpticalLifecycleError::InvalidIdentity.into());
        }
        let drive = self
            .drives
            .get(drive_id)
            .ok_or(OpticalServiceError::UnknownDrive)?;
        let permit = self.manager.claim_playback_request(
            drive_id,
            expected_generation,
            expected_disc_id,
            &title.title_id,
            session_id,
            request_id,
            request_digest,
        )?;
        let permit = match permit {
            OpticalPlaybackClaim::Claimed(permit) => permit,
            OpticalPlaybackClaim::Replay { session_id } => {
                return Ok(OpticalTitleClaim::Replay { session_id })
            }
        };
        let source =
            permit.playback_source(expected_disc_id.to_owned(), title.title_id.clone(), angle)?;
        let input = self.host.resolve_input(drive, title.locator, angle)?;
        Ok(OpticalTitleClaim::Claimed(OpticalPlaybackLease {
            source,
            input,
            permit,
        }))
    }

    /// Execute a previously authenticated/authorized eject against the exact
    /// insertion and, when busy, exact stopped session.
    pub async fn eject(
        &self,
        drive_id: &str,
        expected_generation: &str,
    ) -> Result<(), OpticalServiceError> {
        let drive = self
            .drives
            .get(drive_id)
            .ok_or(OpticalServiceError::UnknownDrive)?;
        self.manager
            .authorize_eject(drive_id, expected_generation)?;
        self.host.eject(drive).await?;
        self.manager.observe_removal(drive_id)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::optical::{OpticalDriveState, OpticalFormat, OpticalTitleLocator};
    use crate::store::{OpticalStore, SqliteStore};
    use crate::testfixtures::optical::{
        optical_drive_fixture, optical_folder_fixture, FakeOpticalEvent, FakeOpticalHost,
    };

    #[tokio::test]
    async fn optical_fake_host_drives_inspection_and_exclusive_playback_lease() {
        let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let host = Arc::new(FakeOpticalHost::default());
        host.push_inspection_for_observed_generation(OpticalFormat::Dvd);
        let folder = optical_folder_fixture(OpticalFormat::Dvd, "unused-generation");
        let mount = folder.root;
        let service = OpticalService::new(
            "node-a",
            vec![optical_drive_fixture("drive-a", mount.clone())],
            store.clone(),
            host.clone(),
            std::time::Duration::from_secs(5),
        );

        let ready = service
            .inspect_insertion("drive-a", 100)
            .await
            .expect("fixture inspection");
        let OpticalDriveState::Ready {
            media_generation,
            disc_id,
        } = ready.state
        else {
            panic!("fixture drive did not become ready")
        };
        let title = store
            .optical_title(&disc_id, "title-1")
            .await
            .expect("title read")
            .expect("stored title");

        let input = ResolvedInput::Dvd {
            path: mount,
            title_number: 1,
            angle: 1,
        };
        host.push_resolution(Ok(input.clone()));
        let lease = service
            .claim_playback_title(
                "drive-a",
                &media_generation,
                &disc_id,
                &title,
                1,
                "session-a",
            )
            .expect("first playback lease");
        assert_eq!(lease.input, input);

        let busy = match service.claim_playback(
            "drive-a",
            &media_generation,
            &disc_id,
            "title-1",
            "session-b",
        ) {
            Ok(_) => panic!("a second reader must be refused"),
            Err(error) => error,
        };
        assert!(matches!(
            busy,
            OpticalServiceError::Lifecycle(OpticalLifecycleError::Busy)
        ));
        drop(lease);

        host.push_resolution(Ok(ResolvedInput::Dvd {
            path: input.path().to_path_buf(),
            title_number: 1,
            angle: 1,
        }));
        let successor = service
            .claim_playback_title(
                "drive-a",
                &media_generation,
                &disc_id,
                &title,
                1,
                "session-c",
            )
            .expect("released drive can be claimed again");
        drop(successor);

        let events = host.events();
        assert!(matches!(
            events.first(),
            Some(FakeOpticalEvent::Inspect { drive_id, .. }) if drive_id == "drive-a"
        ));
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, FakeOpticalEvent::Resolve { .. }))
                .count(),
            2
        );
        assert_eq!(title.locator, OpticalTitleLocator::Dvd { title_number: 1 });
    }

    #[tokio::test]
    async fn media_changed_edge_re_fences_a_swap_without_observed_empty_state() {
        let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let host = Arc::new(FakeOpticalHost::default());
        host.push_inspection_for_observed_generation(OpticalFormat::Dvd);
        let mount = optical_folder_fixture(OpticalFormat::Dvd, "unused-generation").root;
        let service = OpticalService::new(
            "node-a",
            vec![optical_drive_fixture("drive-a", mount.clone())],
            store.clone(),
            host.clone(),
            std::time::Duration::from_secs(5),
        );

        let first = service
            .inspect_insertion("drive-a", 100)
            .await
            .expect("first insertion");
        let OpticalDriveState::Ready {
            media_generation: first_generation,
            disc_id: first_disc_id,
        } = first.state
        else {
            panic!("first insertion did not become ready")
        };
        let title = store
            .optical_title(&first_disc_id, "title-1")
            .await
            .expect("title read")
            .expect("stored title");
        host.push_resolution(Ok(ResolvedInput::Dvd {
            path: mount,
            title_number: 1,
            angle: 1,
        }));
        let stale_lease = service
            .claim_playback_title(
                "drive-a",
                &first_generation,
                &first_disc_id,
                &title,
                1,
                "session-a",
            )
            .expect("first playback lease");

        host.push_presence(Ok(OpticalMediaPresence::Changed));
        host.push_inspection_for_observed_generation(OpticalFormat::Bluray);
        assert!(matches!(
            service.observe_once(200).await.as_slice(),
            [(
                drive_id,
                OpticalServiceError::Lifecycle(OpticalLifecycleError::Busy)
            )] if drive_id == "drive-a"
        ));
        assert!(!stale_lease.permit.is_current());
        assert!(matches!(
            service.manager().snapshot("drive-a").map(|row| row.state),
            Some(OpticalDriveState::Inspecting { .. })
        ));

        drop(stale_lease);
        host.push_presence(Ok(OpticalMediaPresence::Present));
        assert!(service.observe_once(201).await.is_empty());

        let second = service.manager().snapshot("drive-a").expect("drive");
        let OpticalDriveState::Ready {
            media_generation: second_generation,
            disc_id: second_disc_id,
        } = second.state
        else {
            panic!("replacement insertion did not become ready")
        };
        assert_ne!(first_generation, second_generation);
        assert_ne!(first_disc_id, second_disc_id);
    }
}
