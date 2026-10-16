//! Actual three-voter startup and Source installation for runtime contracts.
use super::*;
use axum::{extract::State, http::StatusCode, response::IntoResponse, routing::post, Json, Router};
use futures_util::FutureExt;
use plurx_core::cluster::{
    membership::{FinalizeJoinRequest, MembershipManager, RedeemJoinRequest},
    migration::SelectedStore,
};
use std::{panic::AssertUnwindSafe, pin::Pin};

pub(super) struct SourceContractCluster {
    pub(super) _root: tempfile::TempDir,
    members: tokio::sync::Mutex<Vec<SelectedStore>>,
    observed: startup_observer::MeasuredPeers,
    gateway: Option<tokio::task::JoinHandle<()>>,
    client: Option<Client>,
    uncertain_selection_roots: Vec<PathBuf>,
}

async fn redeem(
    State(manager): State<MembershipManager>,
    Json(request): Json<RedeemJoinRequest>,
) -> axum::response::Response {
    match manager.redeem(&request).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    }
}
async fn finalize(
    State(manager): State<MembershipManager>,
    Json(request): Json<FinalizeJoinRequest>,
) -> axum::response::Response {
    match manager.finalize(&request).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    }
}

async fn committed_version(client: &Client) -> i64 {
    let rows = client
        .query_consistent_map::<SchemaText, _>(
            "SELECT CAST(schema_version AS TEXT) AS value FROM cluster_meta WHERE singleton=1",
            hiqlite::params!(),
        )
        .await
        .expect("actual committed Source version");
    let [row] = rows.as_slice() else {
        panic!("exactly one committed cluster marker");
    };
    row.value.parse().expect("integer committed schema version")
}

impl SourceContractCluster {
    async fn start(&mut self) {
        install_contract_crypto_provider();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("owned join gateway");
        let address = listener.local_addr().expect("join gateway address");
        let first_dir = self._root.path().join("node-1");
        std::fs::create_dir(&first_dir).expect("first Source root");
        drop(
            SqliteStore::open(&first_dir.join("plurx.db"))
                .expect("actual Local SQLite predecessor"),
        );
        let mut config = one_voter_config(&first_dir);
        config.cluster.advertise_host = "localhost".into();
        config.server.bind = address;
        config.cluster.join_url = format!("http://{address}");
        self.uncertain_selection_roots.push(first_dir.clone());
        let first = self
            .observed
            .select(&config, false)
            .await
            .expect("actual first selected voter");
        self.members.get_mut().push(first);
        self.uncertain_selection_roots
            .pop()
            .expect("registered first selection attempt");
        let manager = self.members.get_mut()[0].membership_manager();
        let routes = Router::new()
            .route("/api/v1/cluster/join/redeem", post(redeem))
            .route("/api/v1/cluster/join/finalize", post(finalize))
            .with_state(manager.clone())
            .merge(startup_observer::MeasuredPeers::route(manager.clone()));
        self.gateway = Some(tokio::spawn(async move {
            axum::serve(listener, routes)
                .await
                .expect("owned join gateway server");
        }));
        for id in 2..=3 {
            let directory = self._root.path().join(format!("node-{id}"));
            std::fs::create_dir(&directory).expect("joined Source root");
            let token = manager
                .issue_token(Duration::from_secs(120))
                .await
                .expect("actual signed join invitation");
            let path = directory.join("join.token");
            std::fs::write(&path, format!("{}\n", token.token)).expect("isolated invitation file");
            let mut joined_config = one_voter_config(&directory);
            joined_config.cluster.advertise_host = "localhost".into();
            joined_config.server.bind = format!("127.0.0.1:{}", contract_free_ports(1)[0])
                .parse()
                .expect("distinct clock listener");
            joined_config.cluster.join_token_file = path;
            self.uncertain_selection_roots.push(directory.clone());
            let member = self
                .observed
                .select(&joined_config, true)
                .await
                .expect("actual joined running voter");
            self.members.get_mut().push(member);
            self.uncertain_selection_roots
                .pop()
                .expect("registered joined selection attempt");
        }
        let client = self.members.get_mut()[0]
            .local_client()
            .expect("selected local Raft client");
        let actual = client
            .metrics_db()
            .await
            .expect("applied three-voter roster")
            .membership_config
            .voter_ids()
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(actual.len(), 3);
        let selected = self
            .members
            .get_mut()
            .iter()
            .map(|member| member.identity.raft_id)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            actual, selected,
            "applied voters are exactly the three owned selected members"
        );
        let mut identities = std::collections::BTreeSet::new();
        for member in self.members.get_mut().iter() {
            assert!(
                identities.insert(member.identity.node_id.clone()),
                "three distinct selected identities"
            );
            assert_eq!(
                committed_version(&member.local_client().expect("member client")).await,
                AUTH_SCHEMA_BASELINE_VERSION
            );
        }
        self.client = Some(client);
    }

    pub(super) fn client(&self) -> Client {
        self.client
            .as_ref()
            .expect("started fixture client")
            .clone()
    }
    pub(super) async fn open_store(&self) -> HiqliteAuthStore {
        HiqliteAuthStore::open(
            self.client(),
            &self._root.path().join("contract-client-telemetry.db"),
        )
        .await
        .expect("actual Local predecessor adapter")
    }
    pub(super) async fn install_source(&self) {
        let mut members = self.members.lock().await;
        members[0]
            .store
            .put_setting("sharing_enabled", "true")
            .await
            .expect("saved Source enablement");
        for member in members.iter() {
            member
                .membership_manager()
                .prepare_purpose_master(Arc::clone(&member.credential_key))
                .await
                .expect("actual selected purpose master proof");
        }
        for (index, member) in members.iter_mut().enumerate() {
            let ready = member
                .prepare_source_schema_before_serving()
                .await
                .expect("actual coordinated Source installer");
            assert_eq!(
                ready,
                index == 2,
                "only the final compatible running voter completes Source installation"
            );
            let expected = if index == 2 {
                AUTH_SCHEMA_VERSION
            } else {
                AUTH_SCHEMA_BASELINE_VERSION
            };
            assert_eq!(committed_version(&self.client()).await, expected);
        }
        for member in members.iter() {
            assert!(member
                .membership_manager()
                .source_layout_ready()
                .await
                .expect("each original running voter verifies installed Source"));
        }
    }
    pub(super) async fn selected_member(
        &self,
        index: usize,
    ) -> (String, u64, Arc<plurx_core::secrets::CredentialKey>) {
        let members = self.members.lock().await;
        let member = &members[index];
        (
            member.identity.node_id.clone(),
            member.identity.raft_id,
            Arc::clone(&member.credential_key),
        )
    }

    /// Metadata-only boot used by SQL custody controls; no physical driver exists.
    pub(super) async fn publish_metadata_ingress_boot(&self, boot: uuid::Uuid) {
        let members = self.members.lock().await;
        for member in members.iter() {
            let manager = member.membership_manager();
            manager.set_ingress_custody_boot(Some(boot));
            manager
                .publish_ingress_custody_boot()
                .await
                .expect("selected manager publishes fixture boot");
        }
    }

    async fn drain(&mut self) -> Vec<String> {
        let mut failures = self
            .uncertain_selection_roots
            .iter()
            .map(|path| {
                format!(
                    "unreturned selector has unproved storage drainage: {}",
                    path.display()
                )
            })
            .collect::<Vec<_>>();
        for member in self.members.get_mut().iter() {
            match AssertUnwindSafe(member.shutdown()).catch_unwind().await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => failures.push(format!("voter shutdown refused: {error}")),
                Err(_) => failures.push("voter shutdown panicked".into()),
            }
        }
        if let Some(task) = self.gateway.take() {
            task.abort();
            if let Err(error) = task.await {
                if !error.is_cancelled() {
                    failures.push(format!("join task failed: {error}"));
                }
            }
        }
        failures.extend(self.observed.drain_all().await);
        failures
    }
}

pub(super) async fn with_cluster<F>(body: F)
where
    F: for<'a> FnOnce(&'a SourceContractCluster) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>,
{
    let mut fixture = SourceContractCluster {
        _root: tempfile::tempdir().expect("owned Source three-voter root"),
        members: tokio::sync::Mutex::new(Vec::new()),
        observed: startup_observer::MeasuredPeers::default(),
        gateway: None,
        client: None,
        uncertain_selection_roots: Vec::new(),
    };
    let setup = AssertUnwindSafe(async {
        tokio::time::timeout(Duration::from_secs(120), fixture.start())
            .await
            .expect("Source setup stays inside its owned deadline before drainage");
    })
    .catch_unwind()
    .await;
    let result = match setup {
        Ok(()) => {
            AssertUnwindSafe(async {
                tokio::time::timeout(Duration::from_secs(180), body(&fixture))
                    .await
                    .expect("Source scenario stays inside its owned deadline before drainage");
            })
            .catch_unwind()
            .await
        }
        Err(panic) => Err(panic),
    };
    let failures = fixture.drain().await;
    // A cancelled or panicking installation may retain an activation-file
    // writer beyond voter shutdown. Keep the full owner on every failed body
    // or setup, even when the known voter drains themselves succeeded.
    if result.is_err() || !failures.is_empty() {
        eprintln!(
            "Source fixture retains failed or uncertain storage roots at {}: {failures:?}",
            fixture._root.path().display()
        );
        std::mem::forget(fixture);
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
        panic!("Source fixture did not prove complete drainage: {failures:?}");
    }
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}
