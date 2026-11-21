//! Membership-driven LAN discovery, separate from serving readiness.
use std::future::Future;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::cluster::{Standing, StandingWatch};

pub(crate) const PATH: &str = "/api/v1/server/discovery";

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Eligibility {
    pub(crate) advertise: bool,
}

pub(crate) fn visible(standing: Standing) -> bool {
    matches!(standing, Standing::Standalone | Standing::Voter)
}

/// Typed current standing plus its notification source. Test sources drive the
/// same owner; production only constructs the facade's applied standing watch.
pub(crate) trait StandingSource: Clone + Send + Sync + 'static {
    fn current(&self) -> Standing;
    fn changed(&mut self) -> impl Future<Output = Option<Standing>> + Send;
}
impl StandingSource for StandingWatch {
    fn current(&self) -> Standing {
        StandingWatch::current(self)
    }
    fn changed(&mut self) -> impl Future<Output = Option<Standing>> + Send {
        StandingWatch::changed(self)
    }
}

struct Advertisement(Option<mdns_sd::ServiceDaemon>);
impl Advertisement {
    /// Withdraw the record and keep the daemon handle until the withdrawal
    /// is acknowledged, so an unconfirmed attempt can be retried. A daemon
    /// whose thread has already exited holds no record.
    async fn withdraw(&mut self) -> anyhow::Result<()> {
        let Some(daemon) = self.0.as_ref() else {
            return Ok(());
        };
        let acknowledgement = match daemon.shutdown() {
            Ok(acknowledgement) => acknowledgement,
            Err(mdns_sd::Error::DaemonShutdown) => {
                // Nothing further can be withdrawn through this handle, and
                // this owner did not confirm a goodbye: report it, and let a
                // later promotion register afresh.
                self.0 = None;
                anyhow::bail!(
                    "the Bonjour advertiser had already stopped; its records expire on their own"
                );
            }
            Err(error) => return Err(error).context("shutting down Bonjour advertiser"),
        };
        let status = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            acknowledgement.recv_async(),
        )
        .await
        .context("Bonjour withdrawal acknowledgement timed out")?
        .context("Bonjour withdrawal acknowledgement closed")?;
        anyhow::ensure!(
            matches!(status, mdns_sd::DaemonStatus::Shutdown),
            "Bonjour withdrawal was not acknowledged"
        );
        self.0 = None;
        Ok(())
    }
}
impl Drop for Advertisement {
    fn drop(&mut self) {
        // Cancellation cannot await; the normal path awaits acknowledgement.
        if let Some(daemon) = self.0.take() {
            if let Err(error) = daemon.shutdown() {
                tracing::warn!(%error, "Bonjour discovery shutdown failed");
            }
        }
    }
}

pub(crate) struct Gdm {
    pub(crate) instance_id: String,
    pub(crate) node_id: Option<String>,
    pub(crate) name: String,
    pub(crate) http_port: u16,
    pub(crate) port: u16,
}

/// Own both protocol tasks through normal drain and request cancellation.
pub(crate) struct Owner {
    stop: CancellationToken,
    tasks: [Option<JoinHandle<()>>; 2],
}
impl Owner {
    pub(crate) fn start<S, Register>(
        mut standing: S,
        mut register: Register,
        gdm: Option<Gdm>,
    ) -> Self
    where
        S: StandingSource,
        Register: FnMut() -> Option<mdns_sd::ServiceDaemon> + Send + 'static,
    {
        let stop = CancellationToken::new();
        let gdm_task = gdm.map(|gdm| {
            let standing = standing.clone();
            let stop = stop.clone();
            tokio::spawn(async move {
                tokio::select! {
                    biased;
                    () = stop.cancelled() => {}
                    result = super::gdm_responder(gdm.instance_id, gdm.node_id, gdm.name, gdm.http_port, gdm.port, standing) => {
                        if let Err(error) = result {
                            tracing::warn!(%error, "GDM discovery responder unavailable");
                        }
                    }
                }
            })
        });
        let mdns_stop = stop.clone();
        let mdns_task = tokio::spawn(async move {
            let mut advertisement = Advertisement(None);
            loop {
                if mdns_stop.is_cancelled() {
                    break;
                }
                if visible(standing.current()) {
                    if advertisement.0.is_none() {
                        advertisement.0 = register();
                    }
                } else if let Err(error) = advertisement.withdraw().await {
                    if advertisement.0.is_none() {
                        // The advertiser had already stopped: nothing to retry.
                        tracing::warn!(%error, "Bonjour withdrawal was not confirmed by this owner");
                    } else {
                        // The handle is kept, so no new record races the old
                        // one; retry until the withdrawal is acknowledged or
                        // the owner stops. The GDM task continues on its own.
                        tracing::warn!(%error, "Bonjour withdrawal unconfirmed; retrying");
                        tokio::select! {
                            biased;
                            () = mdns_stop.cancelled() => break,
                            () = tokio::time::sleep(std::time::Duration::from_secs(1)) => {}
                        }
                        continue;
                    }
                }
                tokio::select! {
                    biased;
                    () = mdns_stop.cancelled() => break,
                    next = standing.changed() => {
                        if next.is_none() {
                            // Closed notifications do not assert ineligibility.
                            mdns_stop.cancelled().await;
                            break;
                        }
                    }
                }
            }
            if let Err(error) = advertisement.withdraw().await {
                tracing::warn!(%error, "Bonjour discovery shutdown failed");
            }
        });
        Self {
            stop,
            tasks: [Some(mdns_task), gdm_task],
        }
    }

    #[cfg(test)]
    pub(crate) fn with_advertisement_for_test(daemon: mdns_sd::ServiceDaemon) -> Self {
        let stop = CancellationToken::new();
        let stopped = stop.clone();
        let task = tokio::spawn(async move {
            let mut advertisement = Advertisement(Some(daemon));
            stopped.cancelled().await;
            if let Err(error) = advertisement.withdraw().await {
                tracing::warn!(%error, "Bonjour discovery shutdown failed");
            }
        });
        Self {
            stop,
            tasks: [Some(task), None],
        }
    }

    pub(crate) async fn shutdown(mut self) {
        self.stop.cancel();
        for task in &mut self.tasks {
            if let Some(task) = task.take() {
                if let Err(error) = task.await {
                    tracing::warn!(%error, "discovery owner task failed");
                }
            }
        }
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

pub(crate) async fn eligibility(
    client: &reqwest::Client,
    base: &reqwest::Url,
) -> anyhow::Result<Eligibility> {
    client
        .get(base.join(PATH)?)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await
        .context("decoding discovery eligibility")
}

pub(crate) async fn companion(
    server: &str,
    advertiser: super::MdnsAdvertiser,
    retry_after: std::time::Duration,
    shutdown: impl Future<Output = ()>,
) -> anyhow::Result<()> {
    let (base, port, info_url) = super::discovery_endpoints(server)?;
    let client = super::discovery_client()?;
    tokio::pin!(shutdown);
    loop {
        let Some(info) = super::await_discovery_identity(
            &client,
            &base,
            info_url.clone(),
            retry_after,
            &mut shutdown,
        )
        .await
        else {
            return Ok(());
        };
        let hostname = super::system_hostname();
        let address = super::primary_lan_address();
        let node_id = info.cluster_advertisement.then_some(info.node_id.as_str());
        let name = super::discovery_name(&info.name, hostname.as_deref(), address.as_ref());
        let instance = super::instance_label(&name, node_id);
        let mut advertisement = Advertisement(Some(advertiser(
            &info.instance_id,
            &name.label,
            &instance,
            node_id,
            std::net::SocketAddr::from(([0, 0, 0, 0], port)),
            &info.version,
        )?));
        tracing::info!(server = %base, "host Bonjour discovery companion ready");
        let mut failures = 0_u64;
        loop {
            tokio::select! {
                biased;
                () = &mut shutdown => { advertisement.withdraw().await?; return Ok(()); }
                () = tokio::time::sleep(retry_after) => {}
            }
            let observed = tokio::select! {
                biased;
                () = &mut shutdown => { advertisement.withdraw().await?; return Ok(()); }
                observed = eligibility(&client, &base) => observed,
            };
            match observed {
                Ok(Eligibility { advertise: false }) => {
                    advertisement.withdraw().await?;
                    break;
                }
                Ok(Eligibility { advertise: true }) => failures = 0,
                Err(error) => {
                    // An unreadable answer never revokes a known identity.
                    failures = failures.saturating_add(1);
                    if super::should_log_wait(failures) {
                        tracing::warn!(%error, server = %base, "discovery eligibility unreadable; retaining advertisement");
                    }
                }
            }
        }
    }
}

#[cfg(test)]
impl StandingSource for tokio::sync::watch::Receiver<Standing> {
    fn current(&self) -> Standing {
        *self.borrow()
    }
    async fn changed(&mut self) -> Option<Standing> {
        tokio::sync::watch::Receiver::changed(self).await.ok()?;
        Some(*self.borrow())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicU8, AtomicUsize, Ordering},
        Arc, Mutex,
    };
    use std::time::Duration;

    async fn until(mut ready: impl FnMut() -> bool) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !ready() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("bounded discovery transition");
    }

    async fn daemon_status(
        daemon: &mdns_sd::ServiceDaemon,
    ) -> anyhow::Result<mdns_sd::DaemonStatus> {
        let reply = daemon.status().context("requesting daemon status")?;
        tokio::time::timeout(Duration::from_secs(3), reply.recv_async())
            .await
            .context("bounded daemon status")?
            .context("daemon status reply")
    }

    async fn withdrawn(daemon: &mdns_sd::ServiceDaemon) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                // A status request queued behind Exit can lose its reply.
                // Only an affirmative Shutdown finishes this observation;
                // an unreadable reply is followed by a fresh status query.
                if matches!(
                    daemon_status(daemon).await,
                    Ok(mdns_sd::DaemonStatus::Shutdown)
                ) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("advertisement withdrawn");
    }

    #[test]
    fn only_standalone_and_voters_are_advertised() {
        // `GET /api/v1/server/discovery` answers exactly this for the node's
        // applied standing (`http::system::discovery_eligibility`).
        use crate::cluster::Standing;
        assert!(visible(Standing::Standalone));
        assert!(visible(Standing::Voter));
        assert!(!visible(Standing::Learner), "a learner is never advertised");
        assert!(
            !visible(Standing::Absent),
            "a removed node is never advertised"
        );
    }

    #[tokio::test]
    async fn applied_standing_changes_register_withdraw_and_promote_without_restart() {
        let (source, standing) = tokio::sync::watch::channel(Standing::Learner);
        let registrations = Arc::new(AtomicUsize::new(0));
        let daemons = Arc::new(Mutex::new(Vec::new()));
        let owner = Owner::start(
            standing,
            {
                let registrations = registrations.clone();
                let daemons = daemons.clone();
                move || {
                    let daemon =
                        mdns_sd::ServiceDaemon::new_with_port(0).expect("isolated advertiser");
                    daemons.lock().expect("daemons").push(daemon.clone());
                    registrations.fetch_add(1, Ordering::SeqCst);
                    Some(daemon)
                }
            },
            None,
        );
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(
            registrations.load(Ordering::SeqCst),
            0,
            "a learner never registers"
        );
        source.send_replace(Standing::Voter);
        until(|| registrations.load(Ordering::SeqCst) == 1).await;
        source.send_replace(Standing::Voter);
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(
            registrations.load(Ordering::SeqCst),
            1,
            "unchanged standing never republishes"
        );
        let first = daemons.lock().expect("daemons")[0].clone();
        source.send_replace(Standing::Learner);
        withdrawn(&first).await;
        source.send_replace(Standing::Standalone);
        until(|| registrations.load(Ordering::SeqCst) == 2).await;
        let second = daemons.lock().expect("daemons")[1].clone();
        source.send_replace(Standing::Absent);
        withdrawn(&second).await;
        owner.shutdown().await;
    }

    /// An advertiser whose daemon already stopped cannot be withdrawn by this
    /// owner. While the node is a learner nothing is registered in its place;
    /// a later promotion registers afresh instead of leaving the node
    /// unadvertised until restart.
    #[tokio::test]
    async fn a_stopped_advertiser_is_not_replaced_until_promotion() {
        let (source, standing) = tokio::sync::watch::channel(Standing::Voter);
        let registrations = Arc::new(AtomicUsize::new(0));
        let seen = Arc::new(Mutex::new(None));
        let owner = Owner::start(
            standing,
            {
                let registrations = registrations.clone();
                let seen = seen.clone();
                move || {
                    let daemon =
                        mdns_sd::ServiceDaemon::new_with_port(0).expect("isolated advertiser");
                    *seen.lock().expect("daemon") = Some(daemon.clone());
                    registrations.fetch_add(1, Ordering::SeqCst);
                    Some(daemon)
                }
            },
            None,
        );
        until(|| registrations.load(Ordering::SeqCst) == 1).await;
        let daemon = seen.lock().expect("daemon").clone().expect("advertiser");
        let mut externally_closed = Advertisement(Some(daemon.clone()));
        externally_closed
            .withdraw()
            .await
            .expect("external shutdown acknowledged");
        source.send_replace(Standing::Learner);
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert_eq!(
            registrations.load(Ordering::SeqCst),
            1,
            "a learner never gets a record in place of the stopped one"
        );
        assert!(
            !owner.tasks[0]
                .as_ref()
                .expect("Bonjour owner")
                .is_finished(),
            "the owner keeps following standing"
        );
        source.send_replace(Standing::Voter);
        until(|| registrations.load(Ordering::SeqCst) == 2).await;
        owner.shutdown().await;
    }

    #[tokio::test]
    async fn closed_standing_notifications_preserve_identity_and_shutdown_settles() {
        let (source, standing) = tokio::sync::watch::channel(Standing::Voter);
        let seen = Arc::new(Mutex::new(None));
        let owner = Owner::start(
            standing,
            {
                let seen = seen.clone();
                move || {
                    let daemon =
                        mdns_sd::ServiceDaemon::new_with_port(0).expect("isolated advertiser");
                    *seen.lock().expect("daemon") = Some(daemon.clone());
                    Some(daemon)
                }
            },
            None,
        );
        until(|| seen.lock().expect("daemon").is_some()).await;
        let daemon = seen
            .lock()
            .expect("daemon")
            .clone()
            .expect("registered daemon");
        drop(source);
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(matches!(
            daemon_status(&daemon).await,
            Ok(mdns_sd::DaemonStatus::Running)
        ));
        tokio::time::timeout(Duration::from_secs(5), owner.shutdown())
            .await
            .expect("closed watch still stops");
        withdrawn(&daemon).await;
    }

    #[tokio::test]
    async fn gdm_answers_only_current_client_serving_standing() {
        let (source, standing) = tokio::sync::watch::channel(Standing::Learner);
        let socket = tokio::net::UdpSocket::bind("127.0.0.1:0")
            .await
            .expect("server");
        let address = socket.local_addr().expect("address");
        let mut tasks = tokio::task::JoinSet::new();
        tasks.spawn(async move {
            crate::gdm_serve(
                &socket,
                "standing-node",
                None,
                "Discovery",
                32400,
                &standing,
            )
            .await
        });
        let client = tokio::net::UdpSocket::bind("127.0.0.1:0")
            .await
            .expect("client");
        client.connect(address).await.expect("connect");
        for (standing, answers) in [
            (Standing::Learner, false),
            (Standing::Voter, true),
            (Standing::Learner, false),
            (Standing::Absent, false),
            (Standing::Standalone, true),
        ] {
            source.send_replace(standing);
            client
                .send(b"M-SEARCH * HTTP/1.0\r\n\r\n")
                .await
                .expect("search");
            let mut buf = [0; 2048];
            let result =
                tokio::time::timeout(Duration::from_millis(100), client.recv(&mut buf)).await;
            assert_eq!(result.is_ok(), answers, "{standing:?}");
            if let Ok(result) = result {
                assert!(result.expect("response") > 0);
            }
        }
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
    }

    #[tokio::test]
    async fn companion_hides_learners_retains_unknown_and_observes_promotion() {
        static REGISTERED: AtomicUsize = AtomicUsize::new(0);
        static DAEMONS: Mutex<Vec<mdns_sd::ServiceDaemon>> = Mutex::new(Vec::new());
        fn register(
            _: &str,
            _: &str,
            _: &str,
            _: Option<&str>,
            _: std::net::SocketAddr,
            _: &str,
        ) -> anyhow::Result<mdns_sd::ServiceDaemon> {
            let daemon = mdns_sd::ServiceDaemon::new_with_port(0)?;
            DAEMONS.lock().expect("daemons").push(daemon.clone());
            REGISTERED.fetch_add(1, Ordering::SeqCst);
            Ok(daemon)
        }
        struct HttpState {
            mode: AtomicU8,
            eligibility: AtomicUsize,
            identity: AtomicUsize,
            held: AtomicUsize,
            active: AtomicUsize,
            release: tokio::sync::watch::Sender<bool>,
        }
        struct HeldRequest(Arc<HttpState>);
        impl Drop for HeldRequest {
            fn drop(&mut self) {
                self.0.active.fetch_sub(1, Ordering::SeqCst);
            }
        }
        let state = Arc::new(HttpState {
            mode: AtomicU8::new(0),
            eligibility: AtomicUsize::new(0),
            identity: AtomicUsize::new(0),
            held: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            release: tokio::sync::watch::channel(false).0,
        });
        let app = axum::Router::new()
            .route(PATH, axum::routing::get(|axum::extract::State(s): axum::extract::State<Arc<HttpState>>| async move {
                s.eligibility.fetch_add(1, Ordering::SeqCst);
                match s.mode.load(Ordering::SeqCst) {
                    3 => (axum::http::StatusCode::OK, axum::Json(serde_json::json!({"unknown":true}))),
                    4 => {
                        s.held.fetch_add(1, Ordering::SeqCst);
                        s.active.fetch_add(1, Ordering::SeqCst);
                        let _held = HeldRequest(s.clone());
                        let mut release = s.release.subscribe();
                        release.wait_for(|ready| *ready).await.expect("HTTP fixture release");
                        (axum::http::StatusCode::OK, axum::Json(serde_json::json!({"advertise":true})))
                    }
                    2 => (axum::http::StatusCode::SERVICE_UNAVAILABLE, axum::Json(serde_json::json!({"error":"unavailable"}))),
                    mode => (axum::http::StatusCode::OK, axum::Json(serde_json::json!({"advertise":mode == 1}))),
                }
            }))
            .route("/api/v1/server", axum::routing::get(|axum::extract::State(s): axum::extract::State<Arc<HttpState>>| async move {
                s.identity.fetch_add(1, Ordering::SeqCst);
                axum::Json(serde_json::json!({"name":"Discovery","version":"0.8.0","instance_id":"server-instance","node_id":"node-one","cluster_advertisement":true}))
            })).with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("server");
        let base = format!("http://{}", listener.local_addr().expect("address"));
        let server_stop = CancellationToken::new();
        let companion_stop = CancellationToken::new();
        let mut tasks = tokio::task::JoinSet::new();
        tasks.spawn({
            let stop = server_stop.clone();
            async move {
                axum::serve(listener, app)
                    .with_graceful_shutdown(stop.cancelled_owned())
                    .await
                    .map_err(anyhow::Error::from)
            }
        });
        tasks.spawn({
            let stop = companion_stop.clone();
            async move {
                companion(
                    &base,
                    register,
                    Duration::from_millis(5),
                    stop.cancelled_owned(),
                )
                .await
            }
        });
        until(|| state.eligibility.load(Ordering::SeqCst) >= 3).await;
        assert_eq!(
            state.identity.load(Ordering::SeqCst),
            0,
            "hidden companion never asks the forbidden identity route"
        );
        assert_eq!(REGISTERED.load(Ordering::SeqCst), 0);
        state.mode.store(1, Ordering::SeqCst);
        until(|| REGISTERED.load(Ordering::SeqCst) == 1).await;
        assert_eq!(state.identity.load(Ordering::SeqCst), 1);
        let first = DAEMONS.lock().expect("daemons")[0].clone();
        state.mode.store(2, Ordering::SeqCst);
        let before = state.eligibility.load(Ordering::SeqCst);
        until(|| state.eligibility.load(Ordering::SeqCst) >= before + 3).await;
        assert!(matches!(
            daemon_status(&first).await,
            Ok(mdns_sd::DaemonStatus::Running)
        ));
        assert_eq!(REGISTERED.load(Ordering::SeqCst), 1);
        assert_eq!(state.identity.load(Ordering::SeqCst), 1);
        state.mode.store(3, Ordering::SeqCst);
        let before = state.eligibility.load(Ordering::SeqCst);
        until(|| state.eligibility.load(Ordering::SeqCst) >= before + 3).await;
        assert!(matches!(
            daemon_status(&first).await,
            Ok(mdns_sd::DaemonStatus::Running)
        ));
        state.mode.store(4, Ordering::SeqCst);
        // The second held request proves the production HTTP deadline expired
        // and was retried without withdrawing an established advertisement.
        until(|| state.held.load(Ordering::SeqCst) >= 2).await;
        assert!(matches!(
            daemon_status(&first).await,
            Ok(mdns_sd::DaemonStatus::Running)
        ));
        assert_eq!(REGISTERED.load(Ordering::SeqCst), 1);
        assert_eq!(state.identity.load(Ordering::SeqCst), 1);
        state.mode.store(0, Ordering::SeqCst);
        state.release.send_replace(true);
        withdrawn(&first).await;
        until(|| state.active.load(Ordering::SeqCst) == 0).await;
        state.mode.store(1, Ordering::SeqCst);
        until(|| REGISTERED.load(Ordering::SeqCst) == 2).await;
        let second = DAEMONS.lock().expect("daemons")[1].clone();
        state.release.send_replace(false);
        state.mode.store(4, Ordering::SeqCst);
        let before = state.held.load(Ordering::SeqCst);
        until(|| state.held.load(Ordering::SeqCst) > before).await;
        companion_stop.cancel();
        tokio::time::timeout(Duration::from_secs(1), tasks.join_next())
            .await
            .expect("companion stops")
            .expect("companion task")
            .expect("join")
            .expect("clean shutdown");
        withdrawn(&second).await;
        state.release.send_replace(true);
        server_stop.cancel();
        while let Some(result) = tasks.join_next().await {
            result.expect("join").expect("server stop");
        }
    }
}
