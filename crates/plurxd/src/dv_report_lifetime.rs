//! Authority of the attachments present at a fresh processed publication.
use std::sync::{Arc, Weak};

#[derive(Default)]
pub(crate) struct PublicationOwners {
    owners: Vec<Owner>,
}
struct Owner {
    physical: Weak<()>,
    generation: Option<String>,
    allowed: bool,
}
impl PublicationOwners {
    pub(crate) fn published(
        &mut self,
        physical: &[Arc<()>],
        expected_revision: u64,
        current_revision: u64,
    ) -> bool {
        if expected_revision != current_revision {
            return false;
        }
        let previous = std::mem::take(&mut self.owners);
        self.owners = physical
            .iter()
            .map(|physical| {
                let weak = Arc::downgrade(physical);
                let generation = previous
                    .iter()
                    .find(|owner| owner.physical.ptr_eq(&weak))
                    .and_then(|owner| owner.generation.clone());
                Owner {
                    physical: weak,
                    generation,
                    allowed: true,
                }
            })
            .collect();
        true
    }
    pub(crate) fn authorize(&mut self, physical: &Arc<()>, generation: &str) -> Option<String> {
        let uuid = uuid::Uuid::parse_str(generation).ok()?;
        if uuid.get_version() != Some(uuid::Version::Random) || uuid.to_string() != generation {
            return None;
        }
        let owner = self.owners.iter_mut().find(|owner| {
            owner.allowed
                && owner
                    .physical
                    .upgrade()
                    .is_some_and(|current| Arc::ptr_eq(&current, physical))
        })?;
        match &owner.generation {
            Some(bound) if bound != generation => None,
            Some(bound) => Some(bound.clone()),
            None => {
                owner.generation = Some(generation.into());
                Some(generation.into())
            }
        }
    }
    pub(crate) fn invalidate(&mut self, physical: &Arc<()>) {
        let weak = Arc::downgrade(physical);
        for owner in &mut self.owners {
            if owner.physical.ptr_eq(&weak) {
                owner.allowed = false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn seek_during_owner_lookup_is_checked_before_report_projection() {
        let physical = Arc::new(());
        let generation = uuid::Uuid::new_v4().to_string();
        let state = std::sync::Mutex::new(PublicationOwners::default());
        assert!(state
            .lock()
            .expect("owners")
            .published(std::slice::from_ref(&physical), 0, 0));
        let (checking, checked) = tokio::sync::oneshot::channel();
        let (resume, resumed) = tokio::sync::oneshot::channel();
        let query = async {
            checking.send(()).expect("owner lookup started");
            resumed.await.expect("owner lookup completed");
            // The production response performs this final synchronous gate
            // after its awaited physical-owner lookup.
            state
                .lock()
                .expect("owners")
                .authorize(&physical, &generation)
        };
        let seek = async {
            checked.await.expect("paused at owner lookup");
            state.lock().expect("owners").invalidate(&physical);
            resume.send(()).expect("resume query");
        };
        let (report, ()) = tokio::join!(query, seek);
        assert_eq!(report, None);
    }

    #[test]
    fn publication_survives_prefetch_but_not_seek_replacement_or_another_viewer() {
        let first = Arc::new(());
        let second = Arc::new(());
        let first_id = uuid::Uuid::new_v4().to_string();
        let second_id = uuid::Uuid::new_v4().to_string();
        let mut state = PublicationOwners::default();
        assert!(state.published(&[first.clone(), second.clone()], 0, 0));
        assert_eq!(state.authorize(&first, &first_id), Some(first_id.clone()));
        // Pending next-window production does not replace a committed receipt.
        assert_eq!(state.authorize(&first, &first_id), Some(first_id.clone()));
        assert_eq!(state.authorize(&first, &second_id), None);
        assert_eq!(
            state.authorize(&second, &second_id),
            Some(second_id.clone())
        );
        let replacement = Arc::new(());
        assert_eq!(state.authorize(&replacement, &first_id), None);
        state.invalidate(&first);
        assert_eq!(state.authorize(&first, &first_id), None);
        assert_eq!(
            state.authorize(&second, &second_id),
            Some(second_id.clone())
        );
        assert!(!state.published(&[first.clone(), second.clone()], 0, 1));
        assert_eq!(state.authorize(&first, &first_id), None);
        assert!(state.published(&[first.clone(), second.clone()], 1, 1));
        assert_eq!(state.authorize(&first, &first_id), Some(first_id.clone()));
        assert_eq!(state.authorize(&first, &second_id), None);
        state = PublicationOwners::default(); // refusal drops publication authority
        assert_eq!(state.authorize(&first, &first_id), None);
    }
}
