// Test-only selector owner for an actually applied standalone voter.
// This never synthesizes remote evidence or advertises serving readiness.
use super::observer_core as core;
use core::cluster::membership::MembershipManager;
use core::cluster::migration::{
    select_daemon_store_observing, SelectedStore, StartupClockObserver,
};
use core::config::Config;
use core::error::StoreError;
use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;

pub struct AppliedSingletonObserver;

impl StartupClockObserver for AppliedSingletonObserver {
    fn start(
        &self,
        manager: MembershipManager,
        _node: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), StoreError>> + Send + '_>> {
        Box::pin(async move {
            let roster = manager
                .clock_peers()
                .await
                .map_err(|error| StoreError::Database(error.to_string()))?;
            let applied = roster.membership.as_ref().ok_or_else(|| {
                StoreError::Database("test observer has no actual applied membership".into())
            })?;
            if applied.members != BTreeSet::from([applied.local_node]) || !roster.peers.is_empty() {
                return Err(StoreError::Database(
                    "singleton test observer refuses a real remote member; authenticated transport is required".into()));
            }
            let guard = manager.clock_guard();
            let original = guard
                .roster_for_peer_directory(&roster)
                .map_err(|error| StoreError::Database(error.to_string()))?;
            if !guard.publish(original, BTreeMap::new()) {
                return Err(StoreError::Database(
                    "actual singleton membership changed before publication".into(),
                ));
            }
            Ok(())
        })
    }
}

pub async fn select_applied_singleton(config: &Config) -> Result<SelectedStore, StoreError> {
    Box::pin(select_daemon_store_observing(
        config,
        Some(&AppliedSingletonObserver),
    ))
    .await
}

const CLOCK_PATH: &str = "/_internal/v1/clock";

#[derive(Default)]
pub struct MeasuredPeers {
    managers: std::sync::Mutex<Vec<MembershipManager>>,
    tasks: std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

impl Drop for MeasuredPeers {
    fn drop(&mut self) {
        for task in self.tasks.get_mut().expect("test task ownership") {
            task.abort();
        }
    }
}

impl MeasuredPeers {
    pub fn route(manager: MembershipManager) -> axum::Router {
        axum::Router::new()
            .route(CLOCK_PATH, axum::routing::get(measured_clock))
            .with_state(manager)
    }

    pub async fn select(
        &self,
        config: &Config,
        bind_clock_listener: bool,
    ) -> Result<SelectedStore, StoreError> {
        let observer = MeasuredObserver {
            owner: self,
            bind: bind_clock_listener.then_some(config.server.bind),
        };
        Box::pin(select_daemon_store_observing(config, Some(&observer))).await
    }
}

struct MeasuredObserver<'a> {
    owner: &'a MeasuredPeers,
    bind: Option<std::net::SocketAddr>,
}

impl StartupClockObserver for MeasuredObserver<'_> {
    fn start(
        &self,
        manager: MembershipManager,
        _node: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), StoreError>> + Send + '_>> {
        Box::pin(async move {
            if let Some(bind) = self.bind {
                let listener = tokio::net::TcpListener::bind(bind)
                    .await
                    .map_err(|error| StoreError::Database(error.to_string()))?;
                let route = MeasuredPeers::route(manager.clone());
                self.owner
                    .tasks
                    .lock()
                    .expect("test listener ownership")
                    .push(tokio::spawn(async move {
                        axum::serve(listener, route)
                            .await
                            .expect("test clock listener");
                    }));
            }
            self.owner
                .managers
                .lock()
                .expect("test managers")
                .push(manager.clone());
            self.owner
                .tasks
                .lock()
                .expect("test probe ownership")
                .push(tokio::spawn(async move {
                    loop {
                        measured_round(&manager).await;
                        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    }
                }));
            // The selector's unchanged original deadline bounds this await.
            // Both real local and coordinator guards must observe the newly
            // committed member before the selector attempts promotion.
            loop {
                let ready = self
                    .owner
                    .managers
                    .lock()
                    .expect("test managers")
                    .iter()
                    .all(|manager| manager.clock_guard().acquire().is_ok());
                if ready {
                    return Ok(());
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
    }
}

fn now_ms() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("real test wall clock")
            .as_millis(),
    )
    .expect("wall clock fits")
}

#[derive(serde::Serialize, serde::Deserialize)]
struct MeasuredResponse {
    node_id: String,
    received_unix_ms: i64,
    sent_unix_ms: i64,
}

async fn measured_clock(
    axum::extract::State(manager): axum::extract::State<MembershipManager>,
    headers: axum::http::HeaderMap,
) -> Result<axum::response::Response, axum::http::StatusCode> {
    use axum::http::StatusCode;
    use core::cluster::membership::InternalPeerAuth;
    let received_unix_ms = now_ms();
    let exact = |name: &str| -> Result<String, StatusCode> {
        let mut values = headers.get_all(name).iter();
        let value = values.next().ok_or(StatusCode::UNAUTHORIZED)?;
        if values.next().is_some() {
            return Err(StatusCode::UNAUTHORIZED);
        }
        Ok(value
            .to_str()
            .map_err(|_| StatusCode::UNAUTHORIZED)?
            .to_owned())
    };
    let auth = InternalPeerAuth {
        node_id: exact("x-plurx-cluster-node")?,
        target_node_id: exact("x-plurx-cluster-target")?,
        timestamp_ms: exact("x-plurx-cluster-time-ms")?
            .parse()
            .map_err(|_| StatusCode::UNAUTHORIZED)?,
        nonce: exact("x-plurx-cluster-nonce")?,
        signature: exact("x-plurx-cluster-signature")?,
    };
    if !manager
        .authorize_internal_peer_request(&auth, "GET", CLOCK_PATH, &[])
        .await
        .map_err(|_| StatusCode::UNAUTHORIZED)?
        || !manager
            .revalidate_authenticated_clock_request(&auth)
            .await
            .map_err(|_| StatusCode::UNAUTHORIZED)?
    {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let body = serde_json::to_vec(&MeasuredResponse {
        node_id: auth.target_node_id.clone(),
        received_unix_ms,
        sent_unix_ms: now_ms(),
    })
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut payload = 200_u16.to_be_bytes().to_vec();
    payload.extend_from_slice(&body);
    let signature = manager
        .sign_internal_peer_response(&auth.node_id, &auth.nonce, CLOCK_PATH, &payload)
        .map_err(|_| StatusCode::UNAUTHORIZED)?;
    axum::response::Response::builder()
        .status(StatusCode::OK)
        .header("x-plurx-response-signature", signature)
        .body(axum::body::Body::from(body))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn measured_round(manager: &MembershipManager) {
    use core::cluster::clock::PeerClockOffset;
    let Ok(before) = manager.clock_peers().await else {
        return;
    };
    let guard = manager.clock_guard();
    let Ok(ticket) = guard.roster_for_peer_directory(&before) else {
        return;
    };
    let mut observations = BTreeMap::new();
    for peer in &before.peers {
        let Some(base) = &peer.http_base else {
            return;
        };
        let nonce = uuid::Uuid::new_v4().to_string();
        let t1 = now_ms();
        let Ok(auth) =
            manager.sign_internal_peer_request(&peer.node_id, t1, &nonce, "GET", CLOCK_PATH, &[])
        else {
            return;
        };
        let Ok(response) = reqwest::Client::new()
            .get(format!("{base}{CLOCK_PATH}"))
            .header("x-plurx-cluster-node", &auth.node_id)
            .header("x-plurx-cluster-target", &auth.target_node_id)
            .header("x-plurx-cluster-time-ms", auth.timestamp_ms)
            .header("x-plurx-cluster-nonce", &auth.nonce)
            .header("x-plurx-cluster-signature", &auth.signature)
            .timeout(std::time::Duration::from_secs(2))
            .send()
            .await
        else {
            return;
        };
        if response.status().as_u16() != 200 {
            return;
        }
        let Some(signature) = response
            .headers()
            .get("x-plurx-response-signature")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
        else {
            return;
        };
        let Ok(body) = response.bytes().await else {
            return;
        };
        let t4 = now_ms();
        let mut payload = 200_u16.to_be_bytes().to_vec();
        payload.extend_from_slice(&body);
        if !manager
            .authorize_internal_peer_member_response(
                &peer.node_id,
                &auth.node_id,
                &nonce,
                CLOCK_PATH,
                &payload,
                &signature,
            )
            .await
            .unwrap_or(false)
        {
            return;
        }
        let Ok(answer) = serde_json::from_slice::<MeasuredResponse>(&body) else {
            return;
        };
        if answer.node_id != peer.node_id {
            return;
        }
        let rtt = (t4 - t1) - (answer.sent_unix_ms - answer.received_unix_ms);
        if !(0..=2000).contains(&rtt) {
            return;
        }
        observations.insert(
            peer.node_id.clone(),
            PeerClockOffset::Bounded {
                offset_us: (answer.received_unix_ms - t1 + answer.sent_unix_ms - t4) * 500,
                uncertainty_us: rtt * 500 + 1000,
                observed_at: std::time::Instant::now(),
            },
        );
    }
    let Ok(after) = manager.clock_peers().await else {
        return;
    };
    if before.membership != after.membership || before.peers != after.peers {
        return;
    }
    guard.publish(ticket, observations);
}
