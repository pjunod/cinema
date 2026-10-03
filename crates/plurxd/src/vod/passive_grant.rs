//! Metadata-only passive route policy; reader and owner integration is separate.
use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use tokio::time::Instant;

const TTL: Duration = Duration::from_secs(600);
const PER_USER: usize = 64;
const PER_NODE: usize = 4096;

#[derive(Default)]
pub(super) struct Registry(Mutex<HashMap<String, Weak<Grant>>>);

pub(super) struct Grant {
    user: String,
    player: String,
    request: String,
    state: Mutex<Presence>,
}
struct Presence {
    expires: Instant,
    frontier_ms: i64,
    released: bool,
}
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Refusal {
    InvalidIdentity,
    Unavailable,
    Capacity,
}
impl Registry {
    pub(super) fn reserve(
        &self,
        session: &str,
        user: &str,
        player: &str,
        request: &str,
        resurrection: bool,
    ) -> Result<Arc<Grant>, Refusal> {
        if [session, user, player, request]
            .iter()
            .any(|id| id.trim().is_empty())
        {
            return Err(Refusal::InvalidIdentity);
        }
        let mut grants = self.0.lock().expect("passive admission lock");
        grants.retain(|_, grant| grant.strong_count() > 0);
        if let Some(grant) = grants.get(session).and_then(Weak::upgrade) {
            if !grant.matches(user, player, request) || !grant.live() {
                return Err(Refusal::Unavailable);
            }
            return Ok(grant);
        }
        // A cached durable recipe is never permission to mint a new grant.
        if resurrection {
            return Err(Refusal::Unavailable);
        }
        let user_count = grants
            .values()
            .filter_map(Weak::upgrade)
            .filter(|grant| grant.user == user)
            .count();
        if grants.len() >= PER_NODE || user_count >= PER_USER {
            return Err(Refusal::Capacity);
        }
        let grant = Arc::new(Grant {
            user: user.to_owned(),
            player: player.to_owned(),
            request: request.to_owned(),
            state: Mutex::new(Presence {
                expires: Instant::now() + TTL,
                frontier_ms: 0,
                released: false,
            }),
        });
        grants.insert(session.to_owned(), Arc::downgrade(&grant));
        Ok(grant)
    }
}
impl Grant {
    fn matches(&self, user: &str, player: &str, request: &str) -> bool {
        self.user == user && self.player == player && self.request == request
    }
    pub(super) fn live(&self) -> bool {
        self.frontier().is_some()
    }
    pub(super) fn frontier(&self) -> Option<i64> {
        let state = self.state.lock().expect("passive presence lock");
        (!state.released && Instant::now() < state.expires).then_some(state.frontier_ms)
    }
    pub(super) fn presence(&self, user: &str, player: &str, request: &str) -> bool {
        self.matches(user, player, request) && self.delivered(None)
    }
    pub(super) fn delivered(&self, frontier: Option<i64>) -> bool {
        let now = Instant::now();
        let mut state = self.state.lock().expect("passive presence lock");
        if state.released || now >= state.expires {
            return false;
        }
        state.expires = now + TTL;
        if let Some(frontier) = frontier {
            state.frontier_ms = frontier;
        }
        true
    }
    pub(super) fn release(&self) {
        self.state.lock().expect("passive presence lock").released = true;
    }
}

impl super::VodServe {
    /// Trusted caller must first resolve exact current durable play/owner.
    /// Native terminal projection rechecks local liveness under the same gate.
    #[allow(dead_code)] // Compatibility service ingress follows J0 verification.
    pub(crate) async fn passive_presence(
        &self,
        session: &str,
        user: &str,
        player: &str,
        request: &str,
    ) -> bool {
        let lifecycle = self.shared.session_lifecycle(session);
        let _lifecycle = lifecycle.lock().await;
        let sessions = self.shared.sessions.lock().await;
        sessions
            .get(session)
            .filter(|session| session.tombstone.is_none())
            .and_then(|session| session.passive_grant.as_ref())
            .is_some_and(|grant| grant.presence(user, player, request))
    }
}

#[cfg(test)]
impl super::VodServe {
    pub(crate) async fn install_passive_grant_for_test(
        &self,
        id: &str,
        user: &str,
        player: &str,
        request: &str,
    ) {
        let lifecycle = self.shared.session_lifecycle(id);
        let _lifecycle = lifecycle.lock().await;
        let grant = self
            .shared
            .passive_grants
            .reserve(id, user, player, request, false)
            .expect("test grant admission");
        let mut sessions = self.shared.sessions.lock().await;
        let session = sessions.get_mut(id).expect("test attachment");
        session.supersession_user = user.to_owned();
        session.playback_id = player.to_owned();
        session.passive_grant = Some(grant);
    }
    pub(crate) async fn force_reader_idle_for_test(&self, id: &str) {
        let sessions = self.shared.sessions.lock().await;
        let session = sessions.get(id).expect("test attachment");
        *session.last_touch.lock().expect("touch lock") =
            std::time::Instant::now() - super::SESSION_IDLE_TTL - Duration::from_secs(1);
    }
    pub(crate) async fn expire_passive_grant_for_test(&self, id: &str) {
        let sessions = self.shared.sessions.lock().await;
        let grant = sessions
            .get(id)
            .and_then(|session| session.passive_grant.as_ref())
            .expect("test grant");
        grant.state.lock().expect("presence lock").expires = Instant::now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test(start_paused = true)]
    async fn passive_grant_exact_presence_preserves_real_frontier_and_never_revives_expiry() {
        let registry = Registry::default();
        let grant = registry
            .reserve("session", "user", "player", "request", false)
            .expect("grant reservation");
        assert!(grant.delivered(Some(42000)));
        tokio::time::advance(Duration::from_secs(599)).await;
        assert!(!grant.presence("other", "player", "request"));
        assert!(!grant.presence("user", "other", "request"));
        assert!(!grant.presence("user", "player", "old-request"));
        assert!(grant.presence("user", "player", "request"));
        assert_eq!(grant.frontier(), Some(42000));
        let replay = registry
            .reserve("session", "user", "player", "request", true)
            .expect("grant reservation");
        assert!(Arc::ptr_eq(&grant, &replay));
        tokio::time::advance(TTL).await;
        assert!(!grant.presence("user", "player", "request"));
        assert!(!grant.delivered(Some(99000)));
        assert_eq!(grant.frontier(), None);
        assert!(matches!(
            registry.reserve("session", "user", "player", "request", true),
            Err(Refusal::Unavailable)
        ));
    }
    #[tokio::test]
    async fn passive_grant_quota_reservations_drop_on_abort_without_evicting_existing_grants() {
        let registry = Registry::default();
        let mut owned = Vec::new();
        for n in 0..PER_USER {
            owned.push(
                registry
                    .reserve(
                        &format!("session-{n}"),
                        "user",
                        "player",
                        &format!("request-{n}"),
                        false,
                    )
                    .expect("grant reservation"),
            );
        }
        assert!(matches!(
            registry.reserve("overflow", "user", "player", "request", false),
            Err(Refusal::Capacity)
        ));
        assert!(owned.iter().all(|grant| grant.live()));
        owned.pop();
        let replacement = registry
            .reserve("replacement", "user", "player", "request", false)
            .expect("grant reservation");
        replacement.release();
        assert!(!replacement.presence("user", "player", "request"));
        assert!(matches!(
            registry.reserve("replacement", "user", "player", "request", true),
            Err(Refusal::Unavailable)
        ));
        drop(replacement);
        drop(owned);
        assert!(matches!(
            registry.reserve("replacement", "user", "player", "request", true),
            Err(Refusal::Unavailable)
        ));
        assert!(registry
            .reserve("new", "user", "player", "request", false)
            .is_ok());
    }
    #[tokio::test]
    async fn passive_grant_node_quota_and_identity_collisions_are_bounded() {
        let registry = Registry::default();
        let mut owned = Vec::new();
        for n in 0..PER_NODE {
            owned.push(
                registry
                    .reserve(
                        &format!("s{n}"),
                        &format!("u{}", n / PER_USER),
                        "player",
                        "request",
                        false,
                    )
                    .expect("grant reservation"),
            );
        }
        assert!(matches!(
            registry.reserve("overflow", "different-user", "player", "request", false),
            Err(Refusal::Capacity)
        ));
        assert!(matches!(
            registry.reserve("s0", "different-user", "player", "request", false),
            Err(Refusal::Unavailable)
        ));
        assert!(matches!(
            registry.reserve("s0", "u0", "player", "different-request", false),
            Err(Refusal::Unavailable)
        ));
        assert!(matches!(
            registry.reserve("", "u0", "player", "request", false),
            Err(Refusal::InvalidIdentity)
        ));
    }
}
