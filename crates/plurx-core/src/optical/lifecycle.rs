use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use serde::Serialize;

use super::PlaybackSourceRef;
use crate::config::OpticalDriveConfig;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum OpticalDriveState {
    Empty,
    Inspecting {
        media_generation: String,
    },
    Ready {
        media_generation: String,
        disc_id: String,
    },
    Busy {
        media_generation: String,
        disc_id: String,
        title_id: String,
        session_id: String,
    },
    Failed {
        media_generation: Option<String>,
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OpticalDriveSnapshot {
    pub id: String,
    pub label: String,
    pub owner_node_id: String,
    pub state: OpticalDriveState,
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum OpticalLifecycleError {
    #[error("unknown optical drive")]
    UnknownDrive,
    #[error("the expected optical insertion is no longer present")]
    StaleGeneration,
    #[error("optical drive is empty")]
    Empty,
    #[error("optical drive is already in use")]
    Busy,
    #[error("optical drive is not ready")]
    NotReady,
    #[error("the selected optical disc is no longer present")]
    StaleDisc,
    #[error("optical identifier is invalid")]
    InvalidIdentity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReaderKind {
    Inspection,
    Playback,
}

#[derive(Debug, Clone)]
struct ActiveReader {
    lease_id: u64,
    generation: String,
    kind: ReaderKind,
}

#[derive(Debug, Clone)]
struct DriveSlot {
    label: String,
    state: OpticalDriveState,
    active: Option<ActiveReader>,
    next_lease_id: u64,
}

struct ManagerInner {
    owner_node_id: String,
    drives: Mutex<BTreeMap<String, DriveSlot>>,
}

#[derive(Clone)]
pub struct OpticalDriveManager {
    inner: Arc<ManagerInner>,
}

impl OpticalDriveManager {
    pub fn new(owner_node_id: impl Into<String>, drives: &[OpticalDriveConfig]) -> Self {
        let drives = drives
            .iter()
            .map(|drive| {
                (
                    drive.id.clone(),
                    DriveSlot {
                        label: drive.label.clone(),
                        state: OpticalDriveState::Empty,
                        active: None,
                        next_lease_id: 1,
                    },
                )
            })
            .collect();
        Self {
            inner: Arc::new(ManagerInner {
                owner_node_id: owner_node_id.into(),
                drives: Mutex::new(drives),
            }),
        }
    }

    fn drives(&self) -> MutexGuard<'_, BTreeMap<String, DriveSlot>> {
        self.inner
            .drives
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn snapshots(&self) -> Vec<OpticalDriveSnapshot> {
        self.drives()
            .iter()
            .map(|(id, slot)| OpticalDriveSnapshot {
                id: id.clone(),
                label: slot.label.clone(),
                owner_node_id: self.inner.owner_node_id.clone(),
                state: slot.state.clone(),
            })
            .collect()
    }

    pub fn snapshot(&self, drive_id: &str) -> Option<OpticalDriveSnapshot> {
        self.drives()
            .get(drive_id)
            .map(|slot| OpticalDriveSnapshot {
                id: drive_id.to_owned(),
                label: slot.label.clone(),
                owner_node_id: self.inner.owner_node_id.clone(),
                state: slot.state.clone(),
            })
    }

    /// Start a new insertion epoch. Every signal mints a fresh generation and
    /// revokes any reader from the old media, including an uncertain-change
    /// event where the host cannot prove whether the tray actually changed.
    pub fn observe_insertion(&self, drive_id: &str) -> Result<String, OpticalLifecycleError> {
        let mut drives = self.drives();
        let slot = drives
            .get_mut(drive_id)
            .ok_or(OpticalLifecycleError::UnknownDrive)?;
        let generation = uuid::Uuid::new_v4().to_string();
        slot.active = None;
        slot.state = OpticalDriveState::Inspecting {
            media_generation: generation.clone(),
        };
        Ok(generation)
    }

    /// Fence every open and output capability immediately on removal.
    pub fn observe_removal(&self, drive_id: &str) -> Result<(), OpticalLifecycleError> {
        let mut drives = self.drives();
        let slot = drives
            .get_mut(drive_id)
            .ok_or(OpticalLifecycleError::UnknownDrive)?;
        slot.active = None;
        slot.state = OpticalDriveState::Empty;
        Ok(())
    }

    pub fn claim_inspection(
        &self,
        drive_id: &str,
        expected_generation: &str,
    ) -> Result<OpticalReadPermit, OpticalLifecycleError> {
        let mut drives = self.drives();
        let slot = drives
            .get_mut(drive_id)
            .ok_or(OpticalLifecycleError::UnknownDrive)?;
        let generation = match &slot.state {
            OpticalDriveState::Inspecting { media_generation } => media_generation.clone(),
            OpticalDriveState::Empty => return Err(OpticalLifecycleError::Empty),
            OpticalDriveState::Ready { .. } | OpticalDriveState::Failed { .. } => {
                return Err(OpticalLifecycleError::NotReady)
            }
            OpticalDriveState::Busy { .. } => return Err(OpticalLifecycleError::Busy),
        };
        if generation != expected_generation {
            return Err(OpticalLifecycleError::StaleGeneration);
        }
        Self::claim_reader(
            &self.inner,
            slot,
            drive_id,
            &generation,
            ReaderKind::Inspection,
        )
    }

    pub fn claim_playback(
        &self,
        drive_id: &str,
        expected_generation: &str,
        expected_disc_id: &str,
        title_id: &str,
        session_id: &str,
    ) -> Result<OpticalReadPermit, OpticalLifecycleError> {
        if title_id.is_empty() || session_id.is_empty() {
            return Err(OpticalLifecycleError::InvalidIdentity);
        }
        let mut drives = self.drives();
        let slot = drives
            .get_mut(drive_id)
            .ok_or(OpticalLifecycleError::UnknownDrive)?;
        if slot.active.is_some() {
            return Err(OpticalLifecycleError::Busy);
        }
        let (generation, disc_id) = match &slot.state {
            OpticalDriveState::Ready {
                media_generation,
                disc_id,
            } => (media_generation.clone(), disc_id.clone()),
            OpticalDriveState::Busy { .. } | OpticalDriveState::Inspecting { .. } => {
                return Err(OpticalLifecycleError::Busy)
            }
            OpticalDriveState::Empty => return Err(OpticalLifecycleError::Empty),
            OpticalDriveState::Failed { .. } => return Err(OpticalLifecycleError::NotReady),
        };
        if generation != expected_generation {
            return Err(OpticalLifecycleError::StaleGeneration);
        }
        if disc_id != expected_disc_id {
            return Err(OpticalLifecycleError::StaleDisc);
        }
        let permit = Self::claim_reader(
            &self.inner,
            slot,
            drive_id,
            &generation,
            ReaderKind::Playback,
        )?;
        slot.state = OpticalDriveState::Busy {
            media_generation: generation,
            disc_id,
            title_id: title_id.to_owned(),
            session_id: session_id.to_owned(),
        };
        Ok(permit)
    }

    fn claim_reader(
        inner: &Arc<ManagerInner>,
        slot: &mut DriveSlot,
        drive_id: &str,
        generation: &str,
        kind: ReaderKind,
    ) -> Result<OpticalReadPermit, OpticalLifecycleError> {
        if slot.active.is_some() {
            return Err(OpticalLifecycleError::Busy);
        }
        let lease_id = slot.next_lease_id;
        slot.next_lease_id = slot.next_lease_id.saturating_add(1);
        slot.active = Some(ActiveReader {
            lease_id,
            generation: generation.to_owned(),
            kind,
        });
        Ok(OpticalReadPermit {
            inner: Arc::clone(inner),
            drive_id: drive_id.to_owned(),
            generation: generation.to_owned(),
            lease_id,
            kind,
            released: false,
        })
    }

    /// Check an eject against the exact insertion. Busy ejects require the
    /// caller to name the exact active session it already authorized stopping.
    pub fn authorize_eject(
        &self,
        drive_id: &str,
        expected_generation: &str,
        stopped_session_id: Option<&str>,
    ) -> Result<(), OpticalLifecycleError> {
        let drives = self.drives();
        let slot = drives
            .get(drive_id)
            .ok_or(OpticalLifecycleError::UnknownDrive)?;
        match &slot.state {
            OpticalDriveState::Empty => Err(OpticalLifecycleError::Empty),
            OpticalDriveState::Inspecting { media_generation }
            | OpticalDriveState::Ready {
                media_generation, ..
            }
            | OpticalDriveState::Failed {
                media_generation: Some(media_generation),
                ..
            } => {
                if media_generation == expected_generation {
                    Ok(())
                } else {
                    Err(OpticalLifecycleError::StaleGeneration)
                }
            }
            OpticalDriveState::Busy {
                media_generation,
                session_id,
                ..
            } => {
                if media_generation != expected_generation {
                    Err(OpticalLifecycleError::StaleGeneration)
                } else if stopped_session_id == Some(session_id.as_str()) {
                    Ok(())
                } else {
                    Err(OpticalLifecycleError::Busy)
                }
            }
            OpticalDriveState::Failed {
                media_generation: None,
                ..
            } => Err(OpticalLifecycleError::NotReady),
        }
    }

    /// Validate a progress/control write against the exact active physical
    /// session. Durable disc identity alone is insufficient: a stale client
    /// must not update a new viewer's insertion after a tray swap.
    pub fn authorize_session(
        &self,
        drive_id: &str,
        expected_generation: &str,
        expected_disc_id: &str,
        expected_title_id: &str,
        session_id: &str,
    ) -> Result<(), OpticalLifecycleError> {
        let drives = self.drives();
        let slot = drives
            .get(drive_id)
            .ok_or(OpticalLifecycleError::UnknownDrive)?;
        match &slot.state {
            OpticalDriveState::Busy {
                media_generation,
                disc_id,
                title_id,
                session_id: current_session_id,
            } if media_generation == expected_generation
                && disc_id == expected_disc_id
                && title_id == expected_title_id
                && current_session_id == session_id =>
            {
                Ok(())
            }
            OpticalDriveState::Busy {
                media_generation, ..
            } if media_generation != expected_generation => {
                Err(OpticalLifecycleError::StaleGeneration)
            }
            OpticalDriveState::Busy { .. } => Err(OpticalLifecycleError::Busy),
            OpticalDriveState::Empty => Err(OpticalLifecycleError::Empty),
            _ => Err(OpticalLifecycleError::NotReady),
        }
    }
}

pub struct OpticalReadPermit {
    inner: Arc<ManagerInner>,
    drive_id: String,
    generation: String,
    lease_id: u64,
    kind: ReaderKind,
    released: bool,
}

impl OpticalReadPermit {
    pub fn media_generation(&self) -> &str {
        &self.generation
    }

    pub fn playback_source(
        &self,
        disc_id: impl Into<String>,
        title_id: impl Into<String>,
        angle: u32,
    ) -> Result<PlaybackSourceRef, OpticalLifecycleError> {
        if self.kind != ReaderKind::Playback {
            return Err(OpticalLifecycleError::NotReady);
        }
        let source = PlaybackSourceRef::Optical {
            owner_node_id: self.inner.owner_node_id.clone(),
            drive_id: self.drive_id.clone(),
            media_generation: self.generation.clone(),
            disc_id: disc_id.into(),
            title_id: title_id.into(),
            angle,
        };
        source
            .validate()
            .map_err(|_| OpticalLifecycleError::InvalidIdentity)?;
        Ok(source)
    }

    pub fn publish_ready(mut self, disc_id: &str) -> Result<(), OpticalLifecycleError> {
        if self.kind != ReaderKind::Inspection || disc_id.is_empty() {
            return Err(OpticalLifecycleError::InvalidIdentity);
        }
        let mut drives = self
            .inner
            .drives
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let slot = drives
            .get_mut(&self.drive_id)
            .ok_or(OpticalLifecycleError::UnknownDrive)?;
        if !active_matches(slot, self.lease_id, &self.generation, self.kind) {
            return Err(OpticalLifecycleError::StaleGeneration);
        }
        slot.active = None;
        slot.state = OpticalDriveState::Ready {
            media_generation: self.generation.clone(),
            disc_id: disc_id.to_owned(),
        };
        self.released = true;
        Ok(())
    }

    pub fn publish_failed(mut self, reason: &str) -> Result<(), OpticalLifecycleError> {
        if self.kind != ReaderKind::Inspection || reason.is_empty() || reason.len() > 16 * 1024 {
            return Err(OpticalLifecycleError::InvalidIdentity);
        }
        let mut drives = self
            .inner
            .drives
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let slot = drives
            .get_mut(&self.drive_id)
            .ok_or(OpticalLifecycleError::UnknownDrive)?;
        if !active_matches(slot, self.lease_id, &self.generation, self.kind) {
            return Err(OpticalLifecycleError::StaleGeneration);
        }
        slot.active = None;
        slot.state = OpticalDriveState::Failed {
            media_generation: Some(self.generation.clone()),
            reason: reason.to_owned(),
        };
        self.released = true;
        Ok(())
    }
}

fn active_matches(slot: &DriveSlot, lease_id: u64, generation: &str, kind: ReaderKind) -> bool {
    slot.active.as_ref().is_some_and(|active| {
        active.lease_id == lease_id && active.generation == generation && active.kind == kind
    })
}

impl Drop for OpticalReadPermit {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        let mut drives = self
            .inner
            .drives
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(slot) = drives.get_mut(&self.drive_id) else {
            return;
        };
        if !active_matches(slot, self.lease_id, &self.generation, self.kind) {
            return;
        }
        slot.active = None;
        match (&self.kind, &slot.state) {
            (
                ReaderKind::Playback,
                OpticalDriveState::Busy {
                    media_generation,
                    disc_id,
                    ..
                },
            ) => {
                slot.state = OpticalDriveState::Ready {
                    media_generation: media_generation.clone(),
                    disc_id: disc_id.clone(),
                };
            }
            (ReaderKind::Inspection, OpticalDriveState::Inspecting { .. }) => {
                slot.state = OpticalDriveState::Failed {
                    media_generation: Some(self.generation.clone()),
                    reason: "optical inspection was cancelled".to_owned(),
                };
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn manager() -> OpticalDriveManager {
        OpticalDriveManager::new(
            "node-a",
            &[OpticalDriveConfig {
                id: "drive-a".to_owned(),
                label: "Media room".to_owned(),
                device_path: PathBuf::from("/dev/sr0"),
                mount_path: PathBuf::from("/media/disc"),
            }],
        )
    }

    fn ready(manager: &OpticalDriveManager) -> String {
        let generation = manager.observe_insertion("drive-a").expect("insert");
        manager
            .claim_inspection("drive-a", &generation)
            .expect("inspection")
            .publish_ready("disc-a")
            .expect("publish");
        generation
    }

    #[test]
    fn optical_drive_allows_exactly_one_reader_and_drop_releases_it() {
        let manager = manager();
        let generation = ready(&manager);
        let permit = manager
            .claim_playback("drive-a", &generation, "disc-a", "title-a", "session-a")
            .expect("playback");
        assert!(matches!(
            manager.claim_playback("drive-a", &generation, "disc-a", "title-b", "session-b"),
            Err(OpticalLifecycleError::Busy)
        ));
        drop(permit);
        manager
            .claim_playback("drive-a", &generation, "disc-a", "title-b", "session-b")
            .expect("released playback");
    }

    #[test]
    fn optical_stale_release_cannot_release_a_new_insertion_reader() {
        let manager = manager();
        let first = manager.observe_insertion("drive-a").expect("first insert");
        let stale = manager
            .claim_inspection("drive-a", &first)
            .expect("first inspection");
        manager.observe_removal("drive-a").expect("remove");
        let second = manager.observe_insertion("drive-a").expect("second insert");
        let current = manager
            .claim_inspection("drive-a", &second)
            .expect("second inspection");
        drop(stale);
        assert!(matches!(
            manager.claim_inspection("drive-a", &second),
            Err(OpticalLifecycleError::Busy)
        ));
        current.publish_ready("disc-b").expect("publish current");
    }

    #[test]
    fn optical_eject_is_generation_and_session_checked() {
        let manager = manager();
        let generation = ready(&manager);
        let permit = manager
            .claim_playback("drive-a", &generation, "disc-a", "title-a", "session-a")
            .expect("playback");
        assert_eq!(
            manager.authorize_eject("drive-a", "stale", Some("session-a")),
            Err(OpticalLifecycleError::StaleGeneration)
        );
        assert_eq!(
            manager.authorize_eject("drive-a", &generation, None),
            Err(OpticalLifecycleError::Busy)
        );
        manager
            .authorize_eject("drive-a", &generation, Some("session-a"))
            .expect("exact stop-and-eject");
        drop(permit);
    }
}
