//! Process-local sharing transport. Saved enablement is never a readiness gate.
use crate::state::{clock_ms, AppState};
use plurx_core::{
    config::{SharingEgressConfig, SharingNetworkConfig},
    error::StoreError,
    secrets::CredentialKey,
    sharing_tls::{LiveNodeTls, PinnedEgress, SharingTlsListener},
    store::{keys as setting_keys, stored_switch, Store},
};
use serde::Serialize;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex, RwLock},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Serialize)]
pub(crate) struct CertificateStatus {
    pub spki_sha256: String,
    pub expires_at_ms: i64,
}
#[derive(Clone, Serialize)]
pub(crate) struct ImportTransportStatus {
    pub import_id: uuid::Uuid,
    pub lifecycle_generation: i64,
    pub endpoint_generation: i64,
    pub checked_at_ms: i64,
    pub connection: &'static str,
}
#[derive(Clone, Serialize)]
pub(crate) struct SharingStatus {
    pub listener: &'static str,
    pub listener_address: String,
    pub certificate: Option<CertificateStatus>,
    pub certificate_renewal: &'static str,
    pub serve: &'static str,
    pub outbound: &'static str,
    pub tailscale_node_key_expiry: &'static str,
    pub topology_qualification: &'static str,
    pub imports: Vec<ImportTransportStatus>,
    pub receiver_recovery: ReceiverRecoveryStatus,
}
/// What this node's orphaned-receiver recovery owner is doing. Observations
/// of this process only; a stranded row keeps its durable lineage.
#[derive(Clone, Default, Serialize)]
pub(crate) struct ReceiverRecoveryStatus {
    pub last_scan_at_ms: Option<i64>,
    pub in_flight: Vec<uuid::Uuid>,
    pub retired_total: u64,
    pub stranded: Vec<StrandedReceiver>,
}
#[derive(Clone, Serialize)]
pub(crate) struct StrandedReceiver {
    pub incarnation_id: String,
    pub reason: &'static str,
    pub observed_at_ms: i64,
}
pub(crate) struct SharingManager {
    #[allow(dead_code)] // Receiver HTTP registration follows relay qualification.
    pub(crate) receiver_starts: crate::http::shared_receiver_playback::ReceiverStartRegistry,
    pub key: Arc<CredentialKey>,
    pub network: SharingNetworkConfig,
    key_directory: PathBuf,
    status: RwLock<SharingStatus>,
    lifetime: Mutex<Option<CancellationToken>>,
    catalogue_admission: Arc<CatalogueAdmission>,
    catalogue_cache: Mutex<CatalogueCache>,
    scope_control: Arc<tokio::sync::Semaphore>,
    wake: SharingWake,
    pub(crate) receiver_recovery: Mutex<ReceiverRecoveryStatus>,
}
/// How long the listener loop waits between looks while sharing is off,
/// unqualified or failed. A local enable cuts it short; the bound is for an
/// enable written on another node and for retrying a failed bind or TLS open.
const LISTENER_IDLE_POLL: Duration = Duration::from_secs(5);
/// The longest the claim loop sleeps when no import is due sooner. Matches the
/// takeover gate's idle tick. Replicated settings and import rows have no
/// change feed, so this is how an enable or an import written on another
/// node reaches this one; local writes wake the loop through [`SharingWake`].
const CLAIM_IDLE_POLL: Duration = plurx_core::store::watched_drain::IDLE_TICK_MAX;
/// How long a responding active import waits before its next refresh against
/// the Source. The same 60 s the failure backoff settles at, so a healthy
/// peer is asked no more often than one that has stopped answering, on every
/// node that runs the loop.
const ACTIVE_IMPORT_REFRESH: Duration = Duration::from_secs(60);
/// This node's sharing writes. Each loop has its own `Notify`, so
/// `notify_one` keeps a permit for a loop that is busy when the write lands
/// and the wake is never lost. A write made on another node is seen only on
/// the bounded polls above.
#[derive(Default)]
struct SharingWake {
    /// The listener loop, idle while sharing is off or unqualified.
    listener: tokio::sync::Notify,
    /// The claim loop, asleep until its next import is due.
    claims: tokio::sync::Notify,
}
/// Server-only received facts, retained for cleanup before any B publication
/// await. The authenticated credential is sealed into the upstream capsule;
/// it never crosses B's viewer response or a log boundary.
#[allow(dead_code)]
pub(crate) struct ReceiverSourceStartResult {
    pub summary: plurx_core::sharing::ImportSummary,
    pub credential: plurx_core::secrets::Secret,
    pub viewer_hash: String,
    pub endpoint: plurx_core::sharing::Endpoint,
    pub source: crate::http::sharing_direct_wire::DecodedSourceStart,
}
pub(crate) async fn enabled(store: &dyn Store) -> Result<bool, StoreError> {
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        store.get_setting(setting_keys::SHARING_ENABLED),
    )
    .await
    .map_err(|_| StoreError::Identity("sharing authority unavailable".into()))??;
    Ok(stored_switch(result.as_deref(), false))
}
impl SharingManager {
    /// Dispatch only after B has committed its blocked owner and retained the
    /// Source request identity. A received Source handle must survive a later
    /// B revocation so the owning task can settle its physical obligation.
    #[allow(dead_code)] // Wired by the receiver owner, never by a catalogue read.
    pub(crate) async fn start_file_source(
        &self,
        state: &AppState,
        intent: &plurx_core::sharing_receiver_sessions::ReceiverSessionIntent,
        owner: &plurx_core::sharing_receiver_sessions::ReceiverPendingRenewal,
        request_json: &str,
        connection_lifetime: std::sync::Arc<dyn Send + Sync>,
        retain_dispatch: impl FnOnce(
                &plurx_core::secrets::Secret,
                &str,
                &plurx_core::sharing::Endpoint,
            ) -> Result<(), crate::sharing_client::PeerError>
            + Send,
    ) -> Result<ReceiverSourceStartResult, crate::sharing_client::PeerError> {
        use crate::sharing_client::{PeerConnection, PeerError};
        let expected = receiver_source_request(intent, request_json)?;
        if owner.incarnation_id != intent.recipe.source_request_id.to_string()
            || owner.owner_node_id != state.node_id
        {
            return Err(PeerError::Authentication);
        }
        let import = state
            .store
            .sharing_import(intent.scope.import_id)
            .await
            .map_err(|_| PeerError::Unavailable)?
            .ok_or(PeerError::Unavailable)?;
        let summary = &import.summary;
        if summary.state != "active"
            || summary.source_server_id != intent.scope.source_server_id
            || summary.catalogue_epoch != intent.scope.catalogue_epoch
            || summary.lifecycle_generation != intent.scope.lifecycle_generation
            || summary.assignment_generation < intent.scope.assignment_generation
            || summary.endpoint_generation < intent.scope.endpoint_generation
            || summary.claim_id != intent.scope.claim_id
            || summary.remote_grant_id != Some(intent.scope.remote_grant_id)
        {
            return Err(PeerError::Authentication);
        }
        self.ensure_current(state, summary).await?;
        let local = state
            .store
            .sharing_identity(clock_ms())
            .await
            .map_err(|_| PeerError::Unavailable)?;
        let credential = ImportCredential::open(self, local.server_id, &import)
            .map_err(|_| PeerError::Unavailable)?;
        let source = plurx_core::sharing::SharingIdentity {
            server_id: expected.server_id,
            catalogue_epoch: expected.catalogue_epoch,
            created_at_ms: 0,
        };
        let (mut peer, _) = PeerConnection::verified_with_lifetime(
            self,
            &summary.endpoints,
            &source,
            connection_lifetime,
        )
        .await?;
        // The handshake can park. Reobserve the original login and committed
        // blocked owner immediately before the first non-idempotent send.
        self.ensure_current(state, summary).await?;
        let authority = state
            .store
            .prepare_receiver_session_authority(intent.clone())
            .await
            .map_err(|_| PeerError::Unavailable)?
            .ok_or(PeerError::Authentication)?;
        let mut fresh_owner = owner.clone();
        fresh_owner.now_ms = clock_ms();
        fresh_owner.lease_expires_at_ms = fresh_owner.now_ms.saturating_add(30_000);
        if !state
            .store
            .renew_pending_receiver_session(&authority, &fresh_owner)
            .await
            .map_err(|_| PeerError::Unavailable)?
        {
            return Err(PeerError::Authentication);
        }
        let viewer = state
            .store
            .authorize_share_viewer(
                summary.id,
                intent.recipe.reference.library_id.clone(),
                intent.user_id,
            )
            .await
            .map_err(|_| PeerError::Unavailable)?
            .ok_or(PeerError::Authentication)?;
        // Retain cleanup authentication before the first send, including when
        // no Start response returns. The callback performs no await.
        let endpoint = peer
            .verified_endpoint()
            .ok_or(PeerError::IdentityMismatch)?
            .clone();
        retain_dispatch(&credential.credential, &viewer, &endpoint)?;
        // Durable before the first Start byte: a crash after this point must
        // still be able to authenticate the owed End. Only the exact live
        // owner can record it; a refusal or an uncertain commit sends nothing.
        let envelope = crate::http::shared_receiver_playback::seal_dispatch_capsule(
            &self.key,
            local.server_id,
            intent,
            &credential.credential,
            &viewer,
            &endpoint,
        )
        .map_err(|_| PeerError::Unavailable)?;
        let authority = state
            .store
            .prepare_receiver_session_authority(intent.clone())
            .await
            .map_err(|_| PeerError::Unavailable)?
            .ok_or(PeerError::Authentication)?;
        fresh_owner.now_ms = clock_ms();
        match state
            .store
            .record_receiver_dispatch(
                &authority,
                &plurx_core::sharing_receiver_sessions::ReceiverDispatchRecord {
                    owner: fresh_owner,
                    envelope,
                },
            )
            .await
            .map_err(|_| PeerError::Unavailable)?
        {
            plurx_core::sharing_receiver_sessions::ReceiverSourceWrite::Applied
            | plurx_core::sharing_receiver_sessions::ReceiverSourceWrite::Replay => {}
            plurx_core::sharing_receiver_sessions::ReceiverSourceWrite::Refused => {
                return Err(PeerError::Authentication)
            }
        }
        let reply = peer
            .file_start(&credential.credential, &expected, &viewer, request_json)
            .await?;
        // Do not discard a known Source result after an await. Publication
        // repeats B authority; cleanup retains this handle independently.
        Ok(ReceiverSourceStartResult {
            summary: summary.clone(),
            credential: credential.credential,
            viewer_hash: viewer,
            endpoint,
            source: reply,
        })
    }

    pub fn new(
        key: Arc<CredentialKey>,
        key_directory: PathBuf,
        network: SharingNetworkConfig,
    ) -> Self {
        Self {
            receiver_starts: Default::default(),
            key,
            key_directory,
            status: RwLock::new(SharingStatus {
                listener: "not_started",
                listener_address: network.bind.to_string(),
                certificate: None,
                certificate_renewal: "unknown",
                serve: "unknown",
                outbound: "unknown",
                tailscale_node_key_expiry: "unknown",
                topology_qualification: "pending",
                imports: Vec::new(),
                receiver_recovery: ReceiverRecoveryStatus::default(),
            }),
            network,
            lifetime: Mutex::new(None),
            catalogue_admission: Arc::new(CatalogueAdmission::default()),
            catalogue_cache: Mutex::new(CatalogueCache::default()),
            scope_control: Arc::new(tokio::sync::Semaphore::new(32)),
            wake: SharingWake::default(),
            receiver_recovery: Mutex::new(ReceiverRecoveryStatus::default()),
        }
    }
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn scope_control_available(&self) -> usize {
        self.scope_control.available_permits()
    }
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn catalogue_cache_entries(&self) -> usize {
        self.catalogue_cache.lock().expect("cache").entries.len()
    }
    pub fn status(&self) -> SharingStatus {
        let mut status = self.status.read().expect("sharing status lock").clone();
        status.receiver_recovery = self
            .receiver_recovery
            .lock()
            .expect("receiver recovery status")
            .clone();
        status
    }
    pub fn disable_bodies(&self) {
        if let Some(token) = self
            .lifetime
            .lock()
            .expect("sharing lifetime lock")
            .as_ref()
        {
            token.cancel();
        }
    }
    /// The saved switch was written on this node. Disabling ends this node's
    /// bodies at once; enabling wakes both idle loops, so a local enable never
    /// waits out a poll that exists only for writes made on another node.
    pub fn enablement_written(&self, enabled: bool) {
        if enabled {
            self.wake.listener.notify_one();
            self.wake.claims.notify_one();
        } else {
            self.disable_bodies();
        }
    }
    fn begin_lifetime(&self) -> CancellationToken {
        let mut lifetime = self.lifetime.lock().expect("sharing lifetime lock");
        if lifetime
            .as_ref()
            .is_none_or(CancellationToken::is_cancelled)
        {
            *lifetime = Some(CancellationToken::new());
        }
        lifetime.as_ref().expect("new sharing lifetime").clone()
    }
    pub fn egress(&self) -> PinnedEgress {
        match &self.network.egress {
            SharingEgressConfig::Interface { name } => PinnedEgress::Interface(name.clone()),
            SharingEgressConfig::LocalAddress { address } => PinnedEgress::LocalAddress(*address),
        }
    }
    fn listener_status(&self, status: &'static str) {
        self.status.write().expect("sharing status lock").listener = status;
    }
    pub async fn run(self: Arc<Self>, state: AppState, shutdown: CancellationToken) {
        loop {
            if shutdown.is_cancelled() {
                self.disable_bodies();
                return;
            }
            match enabled(state.store.as_ref()).await {
                Ok(false) => {
                    self.disable_bodies();
                    self.listener_status("disabled");
                }
                Err(_) => {
                    self.disable_bodies();
                    self.listener_status("authority_unavailable");
                }
                Ok(true) => {
                    let lifetime = self.begin_lifetime();
                    // The first host profile is raw TCP Serve to loopback TLS.
                    // An unqualified wildcard must never become a LAN listener.
                    if !self.network.bind.ip().is_loopback() || self.network.bind.port() == 0 {
                        self.listener_status("unqualified_listener_profile");
                    } else {
                        let directory = self.key_directory.clone();
                        let opened = tokio::task::spawn_blocking(move || {
                            LiveNodeTls::open(&directory, clock_ms() / 1000)
                        })
                        .await;
                        match opened {
                            Ok(Ok(identity)) => {
                                let identity = Arc::new(identity);
                                match tokio::net::TcpListener::bind(self.network.bind).await {
                                    Ok(listener) => {
                                        self.listener_status("listening");
                                        let stop = CancellationToken::new();
                                        let serve = crate::serve_http(
                                            SharingTlsListener::new(listener, identity.clone()),
                                            crate::http::sharing::peer_router(state.clone()),
                                            stop.clone().cancelled_owned(),
                                            crate::HttpTimeouts {
                                                shutdown_drain: Duration::ZERO,
                                                ..crate::HTTP_TIMEOUTS
                                            },
                                        );
                                        let observe = self.observe_listener(
                                            state.clone(),
                                            identity,
                                            lifetime,
                                            stop.clone(),
                                            shutdown.clone(),
                                        );
                                        tokio::pin!(serve, observe);
                                        tokio::select! {
                                            _ = &mut serve => { stop.cancel(); }
                                            _ = &mut observe => { stop.cancel(); let _ = serve.await; }
                                        }
                                    }
                                    Err(_) => self.listener_status("bind_unavailable"),
                                }
                            }
                            _ => {
                                self.listener_status("tls_unavailable");
                                self.status
                                    .write()
                                    .expect("sharing status lock")
                                    .certificate_renewal = "repair_required";
                            }
                        }
                    }
                }
            }
            if !self.listener_idle(&shutdown).await {
                self.disable_bodies();
                return;
            }
        }
    }
    /// The listener loop's pause between looks; false means shutdown.
    async fn listener_idle(&self, shutdown: &CancellationToken) -> bool {
        tokio::select! {
            () = shutdown.cancelled() => false,
            () = tokio::time::sleep(LISTENER_IDLE_POLL) => true,
            () = self.wake.listener.notified() => true,
        }
    }
    async fn observe_listener(
        &self,
        state: AppState,
        identity: Arc<LiveNodeTls>,
        lifetime: CancellationToken,
        stop: CancellationToken,
        shutdown: CancellationToken,
    ) {
        let mut next_renewal = tokio::time::Instant::now();
        loop {
            if let Ok((pin, expiry)) = identity.status() {
                self.status
                    .write()
                    .expect("sharing status lock")
                    .certificate = Some(CertificateStatus {
                    spki_sha256: pin,
                    expires_at_ms: expiry.saturating_mul(1000),
                });
            }
            if tokio::time::Instant::now() >= next_renewal {
                let node = identity.clone();
                let renewed =
                    tokio::task::spawn_blocking(move || node.renew(clock_ms() / 1000)).await;
                self.status
                    .write()
                    .expect("sharing status lock")
                    .certificate_renewal = if matches!(renewed, Ok(Ok(()))) {
                    "healthy"
                } else {
                    "repair_required"
                };
                next_renewal = tokio::time::Instant::now() + Duration::from_secs(3600);
            }
            tokio::select! {
                () = shutdown.cancelled() => { self.disable_bodies(); stop.cancel(); return; }
                () = lifetime.cancelled() => { stop.cancel(); return; }
                () = tokio::time::sleep(Duration::from_secs(1)) => {}
            }
            match enabled(state.store.as_ref()).await {
                Ok(true) => {}
                Ok(false) => {
                    self.listener_status("disabled");
                    self.disable_bodies();
                    stop.cancel();
                    return;
                }
                Err(_) => {
                    self.listener_status("authority_unavailable");
                    self.disable_bodies();
                    stop.cancel();
                    return;
                }
            }
        }
    }
}

/// Compare the complete retained client recipe; only the private Source
/// request UUID is replaced. This check precedes peer lookup and any send.
pub(crate) fn receiver_initial_request_supported(request_json: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(request_json) else {
        return false;
    };
    value.is_object()
        && [
            "previous_session_id",
            "control_sequence",
            "reopen_reason",
            "intent",
        ]
        .iter()
        .all(|key| value.get(key).is_none_or(serde_json::Value::is_null))
}

pub(crate) fn receiver_source_request(
    intent: &plurx_core::sharing_receiver_sessions::ReceiverSessionIntent,
    request_json: &str,
) -> Result<crate::http::hls::SourcePlaybackTarget, crate::sharing_client::PeerError> {
    use crate::sharing_client::PeerError;
    let recipe = &intent.recipe;
    if !receiver_initial_request_supported(&recipe.request_json) {
        return Err(PeerError::InvalidResponse);
    }
    let expected = crate::http::hls::SourcePlaybackTarget {
        server_id: recipe.reference.server_id,
        catalogue_epoch: recipe.reference.catalogue_epoch,
        library_id: recipe.reference.library_id.clone(),
        item_id: recipe.reference.item_id.clone(),
        file_id: recipe.file_id.clone(),
        revision: recipe.file_revision.clone(),
    };
    crate::http::validate_source_start_request(request_json.as_bytes(), &expected)
        .map_err(|_| PeerError::InvalidResponse)?;
    let wrapper: serde_json::Value =
        serde_json::from_str(request_json).map_err(|_| PeerError::InvalidResponse)?;
    let mut retained: serde_json::Value =
        serde_json::from_str(&recipe.request_json).map_err(|_| PeerError::InvalidResponse)?;
    retained
        .as_object_mut()
        .ok_or(PeerError::InvalidResponse)?
        .insert(
            "request_id".into(),
            recipe.source_request_id.to_string().into(),
        );
    if wrapper.get("session") != Some(&retained)
        || recipe.parent_login_hash != intent.login_hash
        || recipe.reference.import_id != intent.scope.import_id
        || recipe.reference.server_id != intent.scope.source_server_id
        || recipe.reference.catalogue_epoch != intent.scope.catalogue_epoch
        || recipe.lifecycle_generation != intent.scope.lifecycle_generation
        || intent.scope.libraries.as_slice() != [recipe.reference.library_id.clone()]
    {
        return Err(PeerError::InvalidResponse);
    }
    Ok(expected)
}

/// The canonical private Source Start wrapper for a retained recipe: the
/// original complete client request with only `request_id` replaced by the
/// retained Source request UUID. Ingress and crash recovery share it, so the
/// End a recovered owner sends names exactly the Start that was dispatched.
pub(crate) fn receiver_source_wrapper(
    recipe: &plurx_core::sharing_receiver_sessions::RemoteSourceRecipe,
) -> Result<String, crate::sharing_client::PeerError> {
    use crate::sharing_client::PeerError;
    let target = crate::http::hls::SourcePlaybackTarget {
        server_id: recipe.reference.server_id,
        catalogue_epoch: recipe.reference.catalogue_epoch,
        library_id: recipe.reference.library_id.clone(),
        item_id: recipe.reference.item_id.clone(),
        file_id: recipe.file_id.clone(),
        revision: recipe.file_revision.clone(),
    };
    let mut session: serde_json::Value =
        serde_json::from_str(&recipe.request_json).map_err(|_| PeerError::InvalidResponse)?;
    session
        .as_object_mut()
        .ok_or(PeerError::InvalidResponse)?
        .insert(
            "request_id".into(),
            recipe.source_request_id.to_string().into(),
        );
    Ok(serde_json::json!({"reference":target,"session":session}).to_string())
}

/// Versioned ciphertext payload retains the non-secret pairing identity after
/// the invitation envelope has been erased. It is never a response DTO.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ImportCredential {
    pub version: u8,
    #[serde(deserialize_with = "plurx_core::sharing::wire_secret")]
    pub credential: plurx_core::secrets::Secret,
    #[serde(deserialize_with = "plurx_core::sharing::canonical_uuid")]
    pub invitation_id: uuid::Uuid,
    pub started_at_ms: i64,
    pub recipient_name: String,
    pub claim_deadline_ms: i64,
    pub pending_expires_at_ms: Option<i64>,
}
impl ImportCredential {
    pub fn encode(
        secret: &plurx_core::secrets::Secret,
        invitation: uuid::Uuid,
        started_at_ms: i64,
        recipient_name: &str,
        claim_deadline_ms: i64,
        pending_expires_at_ms: Option<i64>,
    ) -> Result<plurx_core::secrets::Secret, StoreError> {
        #[derive(Serialize)]
        struct Wire<'a> {
            version: u8,
            credential: &'a str,
            invitation_id: uuid::Uuid,
            started_at_ms: i64,
            recipient_name: &'a str,
            claim_deadline_ms: i64,
            pending_expires_at_ms: Option<i64>,
        }
        serde_json::to_string(&Wire {
            version: 1,
            credential: secret.expose(),
            invitation_id: invitation,
            started_at_ms,
            recipient_name,
            claim_deadline_ms,
            pending_expires_at_ms,
        })
        .map(plurx_core::secrets::Secret::from_cleartext)
        .map_err(|_| StoreError::Identity("sharing credential unavailable".into()))
    }
    pub fn open(
        manager: &SharingManager,
        local: uuid::Uuid,
        import: &plurx_core::sharing::StoredImport,
    ) -> Result<Self, StoreError> {
        let secret = manager
            .key
            .open_sharing(
                plurx_core::secrets::SharingSecretPurpose::Credential,
                local,
                import.summary.id,
                &import.credential,
            )
            .map_err(|_| StoreError::Identity("sharing credential unavailable".into()))?;
        let decoded: Self = serde_json::from_str(secret.expose())
            .map_err(|_| StoreError::Identity("sharing credential unavailable".into()))?;
        if decoded.version != 1
            || decoded.started_at_ms < 0
            || decoded.claim_deadline_ms < decoded.started_at_ms
            || decoded
                .pending_expires_at_ms
                .is_some_and(|expiry| expiry < 0)
            || decoded.recipient_name.len() > 128
            || decoded.recipient_name.chars().any(char::is_control)
        {
            return Err(StoreError::Identity(
                "sharing credential unavailable".into(),
            ));
        }
        Ok(decoded)
    }
}
#[derive(serde::Deserialize)]
struct ClaimResponse {
    #[serde(deserialize_with = "plurx_core::sharing::canonical_uuid")]
    grant_id: uuid::Uuid,
    state: String,
    pending_expires_at_ms: i64,
    protocol_min: u8,
    protocol_max: u8,
}
#[derive(serde::Deserialize)]
struct GrantResponse {
    #[serde(deserialize_with = "plurx_core::sharing::canonical_uuid")]
    id: uuid::Uuid,
    #[serde(deserialize_with = "plurx_core::sharing::canonical_uuid")]
    recipient_server_id: uuid::Uuid,
    state: String,
    pending_expires_at_ms: i64,
    credential_generation: i64,
    protocol_min: u8,
    protocol_max: u8,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PeerEndpointManifest {
    #[serde(deserialize_with = "plurx_core::sharing::canonical_uuid")]
    server_id: uuid::Uuid,
    #[serde(deserialize_with = "plurx_core::sharing::canonical_uuid")]
    catalogue_epoch: uuid::Uuid,
    revision: i64,
    endpoints: Vec<plurx_core::sharing::Endpoint>,
}
impl SharingManager {
    async fn refresh_active_import(
        &self,
        state: &AppState,
        import: plurx_core::sharing::StoredImport,
    ) -> Result<(), crate::sharing_client::PeerError> {
        use crate::sharing_client::{PeerConnection, PeerError};
        use plurx_core::sharing::{validate_endpoints, MutationOutcome, SharingIdentity};
        let local = state
            .store
            .sharing_identity(clock_ms())
            .await
            .map_err(|_| PeerError::Unavailable)?;
        let credentials = ImportCredential::open(self, local.server_id, &import)
            .map_err(|_| PeerError::Unavailable)?;
        let source = SharingIdentity {
            server_id: import.summary.source_server_id,
            catalogue_epoch: import.summary.catalogue_epoch,
            created_at_ms: 0,
        };
        let (mut peer, _) =
            PeerConnection::verified(self, &import.summary.endpoints, &source).await?;
        self.ensure_current(state, &import.summary).await?;
        let grant: GrantResponse = peer.grant(&credentials.credential).await?;
        if grant.recipient_server_id != local.server_id
            || Some(grant.id) != import.summary.remote_grant_id
        {
            return Err(PeerError::IdentityMismatch);
        }
        if grant.protocol_min > 1
            || grant.protocol_max < 1
            || grant.protocol_min > grant.protocol_max
        {
            return Err(PeerError::ProtocolUnsupported);
        }
        if grant.state != "active" {
            return Err(PeerError::Unavailable);
        }
        let manifest: PeerEndpointManifest = peer.endpoints(&credentials.credential).await?;
        if manifest.server_id != source.server_id
            || manifest.catalogue_epoch != source.catalogue_epoch
        {
            return Err(PeerError::IdentityMismatch);
        }
        validate_endpoints(&manifest.endpoints).map_err(|_| PeerError::InvalidResponse)?;
        if manifest.revision < 1 {
            return Err(PeerError::InvalidResponse);
        }
        if import
            .summary
            .observed_endpoint_revision
            .is_some_and(|revision| manifest.revision <= revision)
        {
            return Ok(());
        }
        self.ensure_current(state, &import.summary).await?;
        let result = state
            .store
            .set_sharing_import_endpoints(
                import.summary.id,
                import.summary.endpoint_generation,
                manifest.endpoints,
                Some(manifest.revision),
                clock_ms(),
            )
            .await
            .map_err(|_| PeerError::Unavailable)?;
        if result != MutationOutcome::Applied {
            return Err(PeerError::Unavailable);
        }
        Ok(())
    }
    async fn ensure_current(
        &self,
        state: &AppState,
        expected: &plurx_core::sharing::ImportSummary,
    ) -> Result<(), crate::sharing_client::PeerError> {
        use crate::sharing_client::PeerError;
        if !enabled(state.store.as_ref())
            .await
            .map_err(|_| PeerError::Unavailable)?
        {
            return Err(PeerError::Unavailable);
        }
        let current = tokio::time::timeout(
            Duration::from_secs(1),
            state.store.sharing_import(expected.id),
        )
        .await
        .map_err(|_| PeerError::Unavailable)?
        .map_err(|_| PeerError::Unavailable)?
        .ok_or(PeerError::Unavailable)?;
        if current.summary.lifecycle_generation != expected.lifecycle_generation
            || current.summary.endpoint_generation != expected.endpoint_generation
            || current.summary.claim_id != expected.claim_id
            || !matches!(
                current.summary.state.as_str(),
                "claiming" | "pending" | "active"
            )
        {
            return Err(PeerError::Unavailable);
        }
        Ok(())
    }
    /// A resume asked for from outside the claim loop follows a local import
    /// mutation (create, re-pair). The loop is woken so the import joins its
    /// schedule now, not at the poll that exists for imports written elsewhere.
    pub async fn resume_import(
        &self,
        state: &AppState,
        import: plurx_core::sharing::StoredImport,
    ) -> Result<(), crate::sharing_client::PeerError> {
        let result = self.resume_claim(state, import).await;
        self.wake.claims.notify_one();
        result
    }
    async fn resume_claim(
        &self,
        state: &AppState,
        import: plurx_core::sharing::StoredImport,
    ) -> Result<(), crate::sharing_client::PeerError> {
        use crate::sharing_client::{PeerConnection, PeerError};
        use plurx_core::{
            secrets::{Secret, SharingSecretPurpose},
            sharing::*,
        };
        let authority_error = |_| PeerError::Unavailable;
        if !enabled(state.store.as_ref())
            .await
            .map_err(authority_error)?
        {
            return Err(PeerError::Unavailable);
        }
        let local = state
            .store
            .sharing_identity(clock_ms())
            .await
            .map_err(authority_error)?;
        let source = SharingIdentity {
            server_id: import.summary.source_server_id,
            catalogue_epoch: import.summary.catalogue_epoch,
            created_at_ms: 0,
        };
        let credentials =
            ImportCredential::open(self, local.server_id, &import).map_err(authority_error)?;
        let now = clock_ms();
        if now
            >= credentials
                .pending_expires_at_ms
                .unwrap_or(credentials.claim_deadline_ms)
        {
            state
                .store
                .fail_current_share_claim(
                    import.summary.id,
                    import.summary.claim_id,
                    import.summary.lifecycle_generation,
                    now,
                )
                .await
                .map_err(authority_error)?;
            return Ok(());
        }
        let (mut peer, _) =
            PeerConnection::verified(self, &import.summary.endpoints, &source).await?;
        self.ensure_current(state, &import.summary).await?;
        let (grant_id, grant_state, expires, min, max) = if let Some(envelope) = &import.claim {
            let secret = self
                .key
                .open_sharing(
                    SharingSecretPurpose::Claim,
                    local.server_id,
                    import.summary.id,
                    envelope,
                )
                .map_err(|_| PeerError::Unavailable)?;
            let invitation = Invitation::parse(&secret).map_err(authority_error)?;
            #[derive(Serialize)]
            struct Request<'a> {
                invitation_id: uuid::Uuid,
                invitation_secret: &'a str,
                claim_id: uuid::Uuid,
                recipient_server_id: uuid::Uuid,
                recipient_name: &'a str,
                grant_credential: &'a str,
            }
            let name = &credentials.recipient_name;
            let payload = Secret::from_cleartext(
                serde_json::to_string(&Request {
                    invitation_id: invitation.id,
                    invitation_secret: invitation.secret.expose(),
                    claim_id: import.summary.claim_id,
                    recipient_server_id: local.server_id,
                    recipient_name: name,
                    grant_credential: credentials.credential.expose(),
                })
                .map_err(|_| PeerError::InvalidResponse)?,
            );
            match peer.claim::<ClaimResponse>(&payload).await {
                Ok(response) => (
                    response.grant_id,
                    response.state,
                    response.pending_expires_at_ms,
                    response.protocol_min,
                    response.protocol_max,
                ),
                Err(PeerError::Rejected(
                    axum::http::StatusCode::GONE
                    | axum::http::StatusCode::NOT_FOUND
                    | axum::http::StatusCode::CONFLICT,
                )) => {
                    state
                        .store
                        .fail_current_share_claim(
                            import.summary.id,
                            import.summary.claim_id,
                            import.summary.lifecycle_generation,
                            now,
                        )
                        .await
                        .map_err(authority_error)?;
                    return Ok(());
                }
                Err(error) => return Err(error),
            }
        } else {
            let response: GrantResponse = peer.grant(&credentials.credential).await?;
            if response.recipient_server_id != local.server_id
                || Some(response.id) != import.summary.remote_grant_id
            {
                return Err(PeerError::IdentityMismatch);
            }
            (
                response.id,
                response.state,
                response.pending_expires_at_ms,
                response.protocol_min,
                response.protocol_max,
            )
        };
        if min > 1 || max < 1 || min > max {
            return Err(PeerError::ProtocolUnsupported);
        }
        if !enabled(state.store.as_ref())
            .await
            .map_err(authority_error)?
        {
            return Err(PeerError::Unavailable);
        }
        if credentials
            .pending_expires_at_ms
            .is_some_and(|confirmed| confirmed != expires)
        {
            return Err(PeerError::InvalidResponse);
        }
        match grant_state.as_str() {
            "active" | "pending" if grant_state == "active" || expires > now => {
                let clear = ImportCredential::encode(
                    &credentials.credential,
                    credentials.invitation_id,
                    credentials.started_at_ms,
                    &credentials.recipient_name,
                    credentials.claim_deadline_ms,
                    Some(expires),
                )
                .map_err(authority_error)?;
                let sealed = self
                    .key
                    .seal_sharing(
                        SharingSecretPurpose::Credential,
                        local.server_id,
                        import.summary.id,
                        clear.expose(),
                    )
                    .map_err(|_| PeerError::Unavailable)?;
                let result = state
                    .store
                    .settle_share_import_response(ImportClaimReceipt {
                        import_id: import.summary.id,
                        claim_id: import.summary.claim_id,
                        lifecycle_generation: import.summary.lifecycle_generation,
                        grant_id,
                        active: grant_state == "active",
                        credential: sealed,
                        now_ms: clock_ms(),
                    })
                    .await
                    .map_err(authority_error)?;
                if result != MutationOutcome::Applied {
                    return Err(PeerError::Unavailable);
                }
            }
            "pending" | "expired" | "revoked" | "disabled" => {
                state
                    .store
                    .fail_current_share_claim(
                        import.summary.id,
                        import.summary.claim_id,
                        import.summary.lifecycle_generation,
                        clock_ms(),
                    )
                    .await
                    .map_err(authority_error)?;
            }
            _ => return Err(PeerError::InvalidResponse),
        }
        Ok(())
    }
    pub async fn claim_loop(self: Arc<Self>, state: AppState, shutdown: CancellationToken) {
        use futures_util::{stream::FuturesUnordered, StreamExt};
        // Attempts, next due time, and the lifecycle generation the schedule
        // belongs to: a re-paired import is a new claim and does not inherit
        // its predecessor's backoff.
        let mut retry =
            std::collections::HashMap::<uuid::Uuid, (u32, tokio::time::Instant, i64)>::new();
        loop {
            if shutdown.is_cancelled() {
                return;
            }
            // Known only after a pass that read the imports; otherwise the
            // loop sleeps the bounded poll, never on a stale schedule.
            let mut next_due = None;
            if matches!(enabled(state.store.as_ref()).await, Ok(true)) {
                if let Ok(imports) = state.store.sharing_imports().await {
                    self.status
                        .write()
                        .expect("sharing status lock")
                        .imports
                        .retain(|observation| {
                            imports.iter().any(|import| {
                                import.id == observation.import_id
                                    && import.state == "active"
                                    && import.lifecycle_generation
                                        == observation.lifecycle_generation
                                    && import.endpoint_generation == observation.endpoint_generation
                            })
                        });
                    retry.retain(|id, (_, _, generation)| {
                        imports.iter().any(|import| {
                            import.id == *id
                                && import.lifecycle_generation == *generation
                                && matches!(
                                    import.state.as_str(),
                                    "claiming" | "pending" | "active"
                                )
                        })
                    });
                    let mut requests = FuturesUnordered::new();
                    let state = &state;
                    let manager = &self;
                    for summary in imports.into_iter().filter(|import| {
                        matches!(import.state.as_str(), "claiming" | "pending" | "active")
                    }) {
                        if retry
                            .get(&summary.id)
                            .is_some_and(|(_, due, _)| *due > tokio::time::Instant::now())
                        {
                            continue;
                        }
                        let id = summary.id;
                        let generation = summary.lifecycle_generation;
                        let observed =
                            (summary.state == "active").then_some(ImportTransportStatus {
                                import_id: id,
                                lifecycle_generation: summary.lifecycle_generation,
                                endpoint_generation: summary.endpoint_generation,
                                checked_at_ms: 0,
                                connection: "unknown",
                            });
                        requests.push(async move {
                            let result = match state.store.sharing_import(id).await {
                                Ok(Some(import)) if import.summary.state == "active" => {
                                    match manager.resume_rotation(state, import, false).await {
                                        Ok(()) => match state.store.sharing_import(id).await {
                                            Ok(Some(current))
                                                if current.summary.state == "active" =>
                                            {
                                                manager.refresh_active_import(state, current).await
                                            }
                                            _ => Err(crate::sharing_client::PeerError::Unavailable),
                                        },
                                        Err(error) => Err(error),
                                    }
                                }
                                Ok(Some(import)) => manager.resume_claim(state, import).await,
                                _ => Err(crate::sharing_client::PeerError::Unavailable),
                            };
                            (id, generation, result, observed)
                        });
                    }
                    loop {
                        tokio::select! {
                            ()=shutdown.cancelled()=>return,
                            completed=requests.next()=>match completed {
                                Some((id,generation,result,observed))=> {
                                    let active = observed.is_some();
                                    if let Some(mut observation) = observed {
                                        use crate::sharing_client::PeerError;
                                        observation.checked_at_ms = clock_ms();
                                        observation.connection = match &result {
                                            Ok(()) => "responding",
                                            Err(PeerError::IdentityMismatch) => "identity_mismatch",
                                            Err(PeerError::Authentication) => "credential_rejected",
                                            Err(PeerError::ProtocolUnsupported) => "protocol_unsupported",
                                            Err(PeerError::InvalidResponse) => "invalid_response",
                                            _ => "unavailable",
                                        };
                                        let mut status = self.status.write().expect("sharing status lock");
                                        status.imports.retain(|old| old.import_id != id);
                                        if status.imports.len() < 32 { status.imports.push(observation); }
                                    }
                                    let attempts=if result.is_ok(){0}else{retry.get(&id).map_or(1,|(attempts,_,_)|attempts.saturating_add(1))};
                                    retry.insert(id,(attempts,tokio::time::Instant::now()+claim_retry_delay(attempts,active),generation));
                                },
                                None=>break,
                            }
                        }
                    }
                    next_due = retry.values().map(|(_, due, _)| *due).min();
                }
            }
            if !self.claims_idle(next_due, &shutdown).await {
                return;
            }
        }
    }
    /// The claim loop's sleep: until the earliest import is due, a local
    /// enable or import mutation, or the bounded poll, whichever comes first.
    /// False means shutdown.
    async fn claims_idle(
        &self,
        next_due: Option<tokio::time::Instant>,
        shutdown: &CancellationToken,
    ) -> bool {
        let poll = tokio::time::Instant::now() + CLAIM_IDLE_POLL;
        let until = next_due.map_or(poll, |due| due.min(poll));
        tokio::select! {
            () = shutdown.cancelled() => false,
            () = tokio::time::sleep_until(until) => true,
            () = self.wake.claims.notified() => true,
        }
    }
}
/// How long an import waits after a pass before it is looked at again.
/// Failures back off from 10 s to a 60 s ceiling, jittered so the fleet does
/// not retry a lost peer in step. A claim still in flight is followed every
/// 5 s; a responding active import waits [`ACTIVE_IMPORT_REFRESH`].
fn claim_retry_delay(attempts: u32, active: bool) -> Duration {
    if attempts == 0 {
        return if active {
            ACTIVE_IMPORT_REFRESH
        } else {
            Duration::from_secs(5)
        };
    }
    let seconds = (5u64.saturating_mul(1u64 << attempts.min(4))).min(60);
    let jitter = uuid::Uuid::new_v4().as_u128() as u64 % 1000;
    Duration::from_millis(seconds * 1000 + jitter)
}
impl SharingManager {
    pub async fn resume_rotation(
        &self,
        state: &AppState,
        import: plurx_core::sharing::StoredImport,
        start: bool,
    ) -> Result<(), crate::sharing_client::PeerError> {
        use crate::sharing_client::{PeerConnection, PeerError};
        use plurx_core::{
            secrets::{Secret, SharingSecretPurpose},
            sharing::*,
        };
        let unavailable = |phase: &'static str| {
            move |_| {
                tracing::warn!(phase, "sharing rotation failed");
                PeerError::Unavailable
            }
        };
        if !enabled(state.store.as_ref())
            .await
            .map_err(unavailable("enabled"))?
        {
            return Err(PeerError::Unavailable);
        }
        let mut rotation = state
            .store
            .sharing_import_rotation(import.summary.id)
            .await
            .map_err(unavailable("load_rotation"))?;
        if rotation.is_none() && !start {
            return Ok(());
        }
        let grant_id = import
            .summary
            .remote_grant_id
            .ok_or(PeerError::InvalidResponse)?;
        let local = state
            .store
            .sharing_identity(clock_ms())
            .await
            .map_err(unavailable("identity"))?;
        let current = ImportCredential::open(self, local.server_id, &import)
            .map_err(unavailable("open_current_credential"))?;
        if rotation.is_none() {
            let secret = new_secret().map_err(unavailable("new_credential"))?;
            let payload = ImportCredential::encode(
                &secret,
                current.invitation_id,
                current.started_at_ms,
                &current.recipient_name,
                current.claim_deadline_ms,
                current.pending_expires_at_ms,
            )
            .map_err(unavailable("encode_credential"))?;
            let sealed = self
                .key
                .seal_sharing(
                    SharingSecretPurpose::Rotation,
                    local.server_id,
                    import.summary.id,
                    payload.expose(),
                )
                .map_err(|_| PeerError::Unavailable)?;
            let result = state
                .store
                .begin_share_import_rotation(ImportRotation {
                    import_id: import.summary.id,
                    request_id: uuid::Uuid::new_v4(),
                    credential: sealed,
                    lifecycle_generation: import.summary.lifecycle_generation,
                    now_ms: clock_ms(),
                })
                .await
                .map_err(unavailable("begin_rotation"))?;
            if result != MutationOutcome::Applied {
                return Err(PeerError::Rejected(axum::http::StatusCode::CONFLICT));
            }
            rotation = state
                .store
                .sharing_import_rotation(import.summary.id)
                .await
                .map_err(unavailable("reload_rotation"))?;
        }
        let rotation = rotation.ok_or(PeerError::Unavailable)?;
        if rotation.expires_at_ms <= clock_ms() {
            return Err(PeerError::Rejected(axum::http::StatusCode::GONE));
        }
        let replacement = self
            .key
            .open_sharing(
                SharingSecretPurpose::Rotation,
                local.server_id,
                import.summary.id,
                &rotation.credential,
            )
            .map_err(|_| PeerError::Unavailable)?;
        let replacement_credential: ImportCredential =
            serde_json::from_str(replacement.expose()).map_err(|_| PeerError::InvalidResponse)?;
        let source = SharingIdentity {
            server_id: import.summary.source_server_id,
            catalogue_epoch: import.summary.catalogue_epoch,
            created_at_ms: 0,
        };
        let (mut peer, _) = PeerConnection::verified(self, &import.summary.endpoints, &source)
            .await
            .inspect_err(|error| {
                tracing::warn!(?error, phase = "verify_peer", "sharing rotation failed");
            })?;
        self.ensure_current(state, &import.summary)
            .await
            .inspect_err(|error| {
                tracing::warn!(?error, phase = "current_import", "sharing rotation failed");
            })?;
        // Status is the only request allowed with the old credential after a
        // committed swap. Try recovery before issuing another mutation.
        let already = peer
            .rotation_status(&current.credential, rotation.request_id, grant_id)
            .await
            .ok()
            .is_some_and(|response| {
                response.get("confirmed") == Some(&serde_json::Value::Bool(true))
            });
        if !already {
            #[derive(Serialize)]
            struct Request<'a> {
                request_id: uuid::Uuid,
                grant_credential: &'a str,
            }
            let payload = Secret::from_cleartext(
                serde_json::to_string(&Request {
                    request_id: rotation.request_id,
                    grant_credential: replacement_credential.credential.expose(),
                })
                .map_err(|_| PeerError::InvalidResponse)?,
            );
            peer.rotate(&current.credential, &payload)
                .await
                .inspect_err(|error| {
                    tracing::warn!(?error, phase = "peer_swap", "sharing rotation failed");
                })?;
        }
        let confirmed: GrantResponse = peer
            .grant(&replacement_credential.credential)
            .await
            .inspect_err(|error| {
                tracing::warn!(
                    ?error,
                    phase = "confirm_replacement",
                    "sharing rotation failed"
                );
            })?;
        if confirmed.id != grant_id
            || confirmed.recipient_server_id != local.server_id
            || confirmed.state != "active"
            || confirmed.credential_generation < 2
        {
            return Err(PeerError::IdentityMismatch);
        }
        if confirmed.protocol_min > 1
            || confirmed.protocol_max < 1
            || confirmed.protocol_min > confirmed.protocol_max
        {
            return Err(PeerError::ProtocolUnsupported);
        }
        let sealed = self
            .key
            .seal_sharing(
                SharingSecretPurpose::Credential,
                local.server_id,
                import.summary.id,
                replacement.expose(),
            )
            .map_err(|_| PeerError::Unavailable)?;
        if !enabled(state.store.as_ref())
            .await
            .map_err(unavailable("enabled_before_commit"))?
        {
            return Err(PeerError::Unavailable);
        }
        if state
            .store
            .commit_share_import_rotation(
                import.summary.id,
                rotation.request_id,
                import.summary.lifecycle_generation,
                confirmed.credential_generation,
                sealed,
                clock_ms(),
            )
            .await
            .map_err(unavailable("commit_rotation"))?
            != MutationOutcome::Applied
        {
            return Err(PeerError::Rejected(axum::http::StatusCode::CONFLICT));
        }
        Ok(())
    }
}

/// No waiters are retained: unavailable imports cannot build a metadata queue.
struct CatalogueAdmission {
    imports: Mutex<std::collections::BTreeSet<uuid::Uuid>>,
    global: Arc<tokio::sync::Semaphore>,
}
impl Default for CatalogueAdmission {
    fn default() -> Self {
        Self {
            imports: Mutex::new(Default::default()),
            global: Arc::new(tokio::sync::Semaphore::new(4)),
        }
    }
}
struct CataloguePermit {
    admission: Arc<CatalogueAdmission>,
    import: uuid::Uuid,
    _global: tokio::sync::OwnedSemaphorePermit,
}
impl Drop for CataloguePermit {
    fn drop(&mut self) {
        self.admission
            .imports
            .lock()
            .expect("catalogue admission")
            .remove(&self.import);
    }
}
impl CatalogueAdmission {
    fn acquire(
        self: &Arc<Self>,
        import: uuid::Uuid,
    ) -> Result<CataloguePermit, crate::sharing_client::PeerError> {
        // Reserve the import first; cancellation and every early refusal drop it.
        let mut imports = self.imports.lock().expect("catalogue admission");
        if imports.contains(&import) {
            return Err(crate::sharing_client::PeerError::Rejected(
                axum::http::StatusCode::TOO_MANY_REQUESTS,
            ));
        }
        let global = self.global.clone().try_acquire_owned().map_err(|_| {
            crate::sharing_client::PeerError::Rejected(axum::http::StatusCode::TOO_MANY_REQUESTS)
        })?;
        imports.insert(import);
        drop(imports);
        Ok(CataloguePermit {
            admission: self.clone(),
            import,
            _global: global,
        })
    }
}

#[derive(Serialize)]
pub(crate) enum CatalogueRead {
    Libraries,
    Page {
        library: plurx_core::sharing::SourceId,
        parent: Option<plurx_core::sharing::SourceId>,
        q: String,
        cursor: Option<String>,
        limit: usize,
    },
    Batch(plurx_core::sharing_catalogue::MetadataBatch),
    Item(plurx_core::sharing::SourceId),
}
#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) enum CatalogueReply {
    Libraries(Vec<plurx_core::store::sharing_catalogue_source::SourceLibrary>),
    Page(plurx_core::sharing_catalogue::CataloguePeerPage),
    Batch(plurx_core::sharing_catalogue::CataloguePeerBatch),
    Item(plurx_core::sharing_catalogue_details::SourceItemDetails),
}
// Bytes are boxed in an Arc, so the charged payload has no spare Vec capacity.
// Cache hits never substitute for a fresh Source read or receiver authority.
const CATALOGUE_CACHE_BYTES: usize = 32 * 1024 * 1024;
const CATALOGUE_CACHE_TTL: Duration = Duration::from_secs(30);
const CATALOGUE_CACHE_ENTRIES: usize = 2048;
struct CachedCatalogue {
    bytes: Arc<[u8]>,
    inserted: std::time::Instant,
    touched: u64,
}
#[derive(Default)]
struct CatalogueCache {
    entries: std::collections::BTreeMap<[u8; 32], CachedCatalogue>,
    bytes: usize,
    tick: u64,
}
impl CatalogueCache {
    fn remove(&mut self, key: &[u8; 32]) {
        if let Some(entry) = self.entries.remove(key) {
            self.bytes -= entry.bytes.len() + 256;
        }
    }
    fn fresh(&mut self, key: &[u8; 32], now: std::time::Instant) -> Option<Arc<[u8]>> {
        if self
            .entries
            .get(key)
            .is_some_and(|v| now.saturating_duration_since(v.inserted) >= CATALOGUE_CACHE_TTL)
        {
            self.remove(key);
        }
        self.tick = self.tick.checked_add(1).unwrap_or_else(|| {
            self.entries.clear();
            self.bytes = 0;
            0
        });
        let entry = self.entries.get_mut(key)?;
        entry.touched = self.tick;
        Some(entry.bytes.clone())
    }
    fn remember(&mut self, key: [u8; 32], bytes: Vec<u8>, now: std::time::Instant) {
        self.remove(&key);
        let charge = bytes.len() + 256;
        if bytes.len() > 4 * 1024 * 1024 || charge > CATALOGUE_CACHE_BYTES {
            return;
        }
        let expired = self
            .entries
            .iter()
            .filter(|(_, e)| now.saturating_duration_since(e.inserted) >= CATALOGUE_CACHE_TTL)
            .map(|(k, _)| *k)
            .collect::<Vec<_>>();
        for key in expired {
            self.remove(&key);
        }
        while self.bytes + charge > CATALOGUE_CACHE_BYTES
            || self.entries.len() >= CATALOGUE_CACHE_ENTRIES
        {
            let Some(key) = self
                .entries
                .iter()
                .min_by_key(|(_, e)| e.touched)
                .map(|(k, _)| *k)
            else {
                return;
            };
            self.remove(&key);
        }
        self.tick = self.tick.checked_add(1).unwrap_or_else(|| {
            self.entries.clear();
            self.bytes = 0;
            0
        });
        self.bytes += charge;
        self.entries.insert(
            key,
            CachedCatalogue {
                bytes: Arc::from(bytes.into_boxed_slice()),
                inserted: now,
                touched: self.tick,
            },
        );
    }
}
fn catalogue_cache_key(
    summary: &plurx_core::sharing::ImportSummary,
    user: i64,
    request: &CatalogueRead,
) -> Result<[u8; 32], crate::sharing_client::PeerError> {
    use sha2::{Digest, Sha256};
    struct Writer(Sha256);
    impl std::io::Write for Writer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Writer(Sha256::new());
    serde_json::to_writer(
        &mut writer,
        &(
            "cinema-receiver-catalogue-cache-v1",
            summary.id,
            summary.source_server_id,
            summary.catalogue_epoch,
            summary.lifecycle_generation,
            summary.assignment_generation,
            summary.endpoint_generation,
            summary.claim_id,
            summary.remote_grant_id,
            user,
            request,
        ),
    )
    .map_err(|_| crate::sharing_client::PeerError::InvalidResponse)?;
    Ok(writer.0.finalize().into())
}
impl SharingManager {
    /// The current active import, the viewer's assignment to the file's
    /// library and a verified pinned connection to its Source. Shared by every
    /// pre-session file read; no offline reply, Local Source ID, B account
    /// identity or session is sent.
    async fn assigned_file_peer(
        &self,
        state: &AppState,
        user: i64,
        reference: &plurx_core::sharing_file_locators::FileLocatorReference,
    ) -> Result<
        (
            plurx_core::sharing::ImportSummary,
            ImportCredential,
            crate::sharing_client::PeerConnection,
        ),
        crate::sharing_client::PeerError,
    > {
        use crate::sharing_client::{PeerConnection, PeerError};
        if user <= 0 {
            return Err(PeerError::Authentication);
        }
        let import = state
            .store
            .sharing_import(reference.item.import_id)
            .await
            .map_err(|_| PeerError::Unavailable)?
            .ok_or(PeerError::Unavailable)?;
        let summary = &import.summary;
        if summary.state != "active"
            || summary.lifecycle_generation != reference.lifecycle_generation
            || summary.source_server_id != reference.item.server_id
            || summary.catalogue_epoch != reference.item.catalogue_epoch
        {
            return Err(PeerError::Authentication);
        }
        self.ensure_current(state, summary).await?;
        let assigned = state
            .store
            .assigned_catalogue_libraries(
                summary.id,
                user,
                summary.lifecycle_generation,
                summary.assignment_generation,
            )
            .await
            .map_err(|_| PeerError::Unavailable)?;
        if !assigned.contains(&reference.item.library_id) {
            return Err(PeerError::Authentication);
        }
        let local = state
            .store
            .sharing_identity(clock_ms())
            .await
            .map_err(|_| PeerError::Unavailable)?;
        let credentials = ImportCredential::open(self, local.server_id, &import)
            .map_err(|_| PeerError::Unavailable)?;
        let expected = plurx_core::sharing::SharingIdentity {
            server_id: summary.source_server_id,
            catalogue_epoch: summary.catalogue_epoch,
            created_at_ms: 0,
        };
        let (peer, _) = PeerConnection::verified(self, &summary.endpoints, &expected).await?;
        self.ensure_current(state, summary).await?;
        Ok((summary.clone(), credentials, peer))
    }

    /// Fresh assigned-file preflight through the approved pinned Source only.
    pub async fn read_file_decision(
        &self,
        state: &AppState,
        user: i64,
        reference: &plurx_core::sharing_file_locators::FileLocatorReference,
        request: &crate::http::shared_playback::SourceDecisionRequest,
    ) -> Result<
        (
            plurx_core::sharing::ImportSummary,
            crate::http::shared_playback::SourceDecisionReply,
        ),
        crate::sharing_client::PeerError,
    > {
        use crate::sharing_client::PeerError;
        tokio::time::timeout(Duration::from_secs(15), async {
            let _permit = self.catalogue_admission.acquire(reference.item.import_id)?;
            let (summary, credentials, mut peer) =
                self.assigned_file_peer(state, user, reference).await?;
            let reply = peer.file_decision(&credentials.credential, request).await?;
            self.ensure_current(state, &summary).await?;
            Ok((summary, reply))
        })
        .await
        .map_err(|_| PeerError::Unavailable)?
    }

    /// A fresh pinned read of one closed pre-session file asset (subtitle
    /// WebVTT, PGS overlay manifest or object, chapter thumbnail). The caller
    /// owns admission; nothing here caches or writes B state.
    pub(crate) async fn read_file_asset(
        &self,
        state: &AppState,
        user: i64,
        reference: &plurx_core::sharing_file_locators::FileLocatorReference,
        target: &crate::http::hls::SourcePlaybackTarget,
        resource: &plurx_core::sharing_resources::SharingFileResource,
    ) -> Result<
        (
            plurx_core::sharing::ImportSummary,
            crate::sharing_client::PeerFileAsset,
        ),
        crate::sharing_client::PeerError,
    > {
        use crate::sharing_client::PeerError;
        tokio::time::timeout(Duration::from_secs(35), async {
            let (summary, credentials, mut peer) =
                self.assigned_file_peer(state, user, reference).await?;
            let asset = peer
                .file_asset(&credentials.credential, target, resource)
                .await?;
            self.ensure_current(state, &summary).await?;
            Ok((summary, asset))
        })
        .await
        .map_err(|_| PeerError::Unavailable)?
    }

    /// A fresh pinned read of the exact opaque artwork resource. No offline
    /// cache hit can substitute for this opened-byte digest proof.
    pub async fn read_artwork(
        &self,
        state: &AppState,
        reference: &plurx_core::sharing_artwork::ReceiverArtReference,
    ) -> Result<
        (
            plurx_core::sharing::ImportSummary,
            crate::sharing_client::PeerArtwork,
        ),
        crate::sharing_client::PeerError,
    > {
        use crate::sharing_client::{PeerConnection, PeerError};
        tokio::time::timeout(Duration::from_secs(5), async {
            let import = state
                .store
                .sharing_import(reference.item.import_id)
                .await
                .map_err(|_| PeerError::Unavailable)?
                .ok_or(PeerError::Unavailable)?;
            let summary = &import.summary;
            let source = reference
                .source
                .reference_unverified()
                .map_err(|_| PeerError::InvalidResponse)?;
            if summary.state != "active"
                || summary.lifecycle_generation != reference.lifecycle_generation
                || summary.source_server_id != reference.item.server_id
                || summary.catalogue_epoch != reference.item.catalogue_epoch
                || summary.remote_grant_id != Some(source.grant_id)
            {
                return Err(PeerError::Authentication);
            }
            self.ensure_current(state, summary).await?;
            let assigned = state
                .store
                .assigned_catalogue_libraries(
                    summary.id,
                    reference.user_id,
                    summary.lifecycle_generation,
                    summary.assignment_generation,
                )
                .await
                .map_err(|_| PeerError::Unavailable)?;
            if !assigned.contains(&reference.item.library_id) {
                return Err(PeerError::Authentication);
            }
            let local = state
                .store
                .sharing_identity(clock_ms())
                .await
                .map_err(|_| PeerError::Unavailable)?;
            let credentials = ImportCredential::open(self, local.server_id, &import)
                .map_err(|_| PeerError::Unavailable)?;
            let expected = plurx_core::sharing::SharingIdentity {
                server_id: summary.source_server_id,
                catalogue_epoch: summary.catalogue_epoch,
                created_at_ms: 0,
            };
            let (mut peer, _) =
                PeerConnection::verified(self, &summary.endpoints, &expected).await?;
            self.ensure_current(state, summary).await?;
            let asset = peer
                .artwork(&credentials.credential, &reference.source)
                .await?;
            Ok((summary.clone(), asset))
        })
        .await
        .map_err(|_| PeerError::Unavailable)?
    }
    /// A bounded live body-authority check, independent of catalogue permits.
    /// It proves no revision, cached bytes, worker or write admission.
    pub async fn current_catalogue_scope(
        &self,
        state: &AppState,
        captured: &plurx_core::store::sharing_catalogue::ReceiverCatalogueScope,
        items: Vec<plurx_core::sharing_catalogue_details::SourceScopeItem>,
        files: Vec<plurx_core::sharing_catalogue_details::SourceScopeFile>,
    ) -> bool {
        use crate::sharing_client::{PeerConnection, PeerError};
        use plurx_core::{sharing::SharingIdentity, sharing_catalogue_details::SourceScopeRequest};
        let Ok(_permit) = self.scope_control.clone().try_acquire_owned() else {
            return false;
        };
        tokio::time::timeout(Duration::from_secs(1), async {
            let import = state
                .store
                .sharing_import(captured.import_id)
                .await
                .map_err(|_| PeerError::Unavailable)?
                .ok_or(PeerError::Unavailable)?;
            let s = &import.summary;
            if s.state != "active"
                || s.source_server_id != captured.source_server_id
                || s.catalogue_epoch != captured.catalogue_epoch
                || s.lifecycle_generation != captured.lifecycle_generation
                || s.assignment_generation < captured.assignment_generation
                || s.endpoint_generation < captured.endpoint_generation
                || s.claim_id != captured.claim_id
                || s.remote_grant_id != Some(captured.remote_grant_id)
            {
                return Err(PeerError::Authentication);
            }
            if !enabled(state.store.as_ref())
                .await
                .map_err(|_| PeerError::Unavailable)?
            {
                return Err(PeerError::Unavailable);
            }
            let local = state
                .store
                .sharing_identity(clock_ms())
                .await
                .map_err(|_| PeerError::Unavailable)?;
            let credentials = ImportCredential::open(self, local.server_id, &import)
                .map_err(|_| PeerError::Unavailable)?;
            let expected = SharingIdentity {
                server_id: s.source_server_id,
                catalogue_epoch: s.catalogue_epoch,
                created_at_ms: 0,
            };
            let (mut peer, _) = PeerConnection::verified(self, &s.endpoints, &expected).await?;
            if !enabled(state.store.as_ref())
                .await
                .map_err(|_| PeerError::Unavailable)?
            {
                return Err(PeerError::Unavailable);
            }
            let request = SourceScopeRequest {
                server_id: s.source_server_id,
                catalogue_epoch: s.catalogue_epoch,
                grant_id: captured.remote_grant_id,
                recipient_server_id: local.server_id,
                libraries: captured.libraries.clone(),
                items,
                files,
            };
            peer.current_scope(&credentials.credential, &request).await
        })
        .await
        .is_ok_and(|result| matches!(result, Ok(true)))
    }
    pub async fn read_admin_libraries(
        &self,
        state: &AppState,
        import_id: uuid::Uuid,
        user: i64,
        hash: &str,
    ) -> Result<
        (
            plurx_core::sharing::ImportSummary,
            Vec<plurx_core::store::sharing_catalogue_source::SourceLibrary>,
        ),
        crate::sharing_client::PeerError,
    > {
        use crate::sharing_client::{PeerConnection, PeerError};
        tokio::time::timeout(Duration::from_secs(8), async {
            let _permit = self.catalogue_admission.acquire(import_id)?;
            let import = tokio::time::timeout(
                Duration::from_secs(1),
                state.store.sharing_import(import_id),
            )
            .await
            .map_err(|_| PeerError::Unavailable)?
            .map_err(|_| PeerError::Unavailable)?
            .ok_or(PeerError::Unavailable)?;
            let summary = &import.summary;
            let scope = plurx_core::store::sharing_catalogue::ReceiverCatalogueScope {
                import_id,
                source_server_id: summary.source_server_id,
                catalogue_epoch: summary.catalogue_epoch,
                lifecycle_generation: summary.lifecycle_generation,
                assignment_generation: summary.assignment_generation,
                endpoint_generation: summary.endpoint_generation,
                claim_id: summary.claim_id,
                remote_grant_id: summary.remote_grant_id.ok_or(PeerError::Unavailable)?,
                libraries: vec![],
            };
            self.ensure_current(state, summary).await?;
            if !state
                .store
                .receiver_admin_catalogue_authorized(hash, user, &scope, clock_ms() / 1000)
                .await
                .map_err(|_| PeerError::Unavailable)?
            {
                return Err(PeerError::Authentication);
            }
            let local = state
                .store
                .sharing_identity(clock_ms())
                .await
                .map_err(|_| PeerError::Unavailable)?;
            let credentials = ImportCredential::open(self, local.server_id, &import)
                .map_err(|_| PeerError::Unavailable)?;
            let expected = plurx_core::sharing::SharingIdentity {
                server_id: summary.source_server_id,
                catalogue_epoch: summary.catalogue_epoch,
                created_at_ms: 0,
            };
            let (mut peer, _) =
                PeerConnection::verified(self, &summary.endpoints, &expected).await?;
            self.ensure_current(state, summary).await?;
            let libraries = peer
                .catalogue_libraries(&credentials.credential)
                .await?
                .libraries;
            if libraries
                .iter()
                .map(|l| &l.library_id)
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != libraries.len()
            {
                return Err(PeerError::InvalidResponse);
            }
            self.ensure_current(state, summary).await?;
            if !state
                .store
                .receiver_admin_catalogue_authorized(hash, user, &scope, clock_ms() / 1000)
                .await
                .map_err(|_| PeerError::Unavailable)?
            {
                return Err(PeerError::Authentication);
            }
            Ok((import.summary, libraries))
        })
        .await
        .map_err(|_| PeerError::Unavailable)?
    }

    pub async fn read_catalogue(
        &self,
        state: &AppState,
        import_id: uuid::Uuid,
        user: i64,
        request: CatalogueRead,
    ) -> Result<
        (plurx_core::sharing::ImportSummary, CatalogueReply),
        crate::sharing_client::PeerError,
    > {
        tokio::time::timeout(
            Duration::from_secs(8),
            self.read_catalogue_inner(state, import_id, user, request),
        )
        .await
        .map_err(|_| crate::sharing_client::PeerError::Unavailable)?
    }
    async fn read_catalogue_inner(
        &self,
        state: &AppState,
        import_id: uuid::Uuid,
        user: i64,
        request: CatalogueRead,
    ) -> Result<
        (plurx_core::sharing::ImportSummary, CatalogueReply),
        crate::sharing_client::PeerError,
    > {
        use crate::sharing_client::{PeerConnection, PeerError};
        use plurx_core::sharing::SharingIdentity;
        if user <= 0 {
            return Err(PeerError::Authentication);
        }
        let _permit = self.catalogue_admission.acquire(import_id)?;
        let import = tokio::time::timeout(
            Duration::from_secs(1),
            state.store.sharing_import(import_id),
        )
        .await
        .map_err(|_| PeerError::Unavailable)?
        .map_err(|_| PeerError::Unavailable)?
        .ok_or(PeerError::Unavailable)?;
        if import.summary.state != "active" {
            return Err(PeerError::Unavailable);
        }
        self.ensure_current(state, &import.summary).await?;
        let assigned = state
            .store
            .assigned_catalogue_libraries(
                import_id,
                user,
                import.summary.lifecycle_generation,
                import.summary.assignment_generation,
            )
            .await
            .map_err(|_| PeerError::Unavailable)?;
        if assigned.is_empty() {
            return Err(PeerError::Authentication);
        }
        if let CatalogueRead::Page { library, .. } = &request {
            if !assigned.contains(library) {
                return Err(PeerError::Authentication);
            }
        }
        match &request {
            CatalogueRead::Page {
                q, cursor, limit, ..
            } if q.len() > 512
                || q.chars().any(char::is_control)
                || cursor.as_ref().is_some_and(|c| c.len() > 4096)
                || !(1..=200).contains(limit) =>
            {
                return Err(PeerError::InvalidResponse)
            }
            CatalogueRead::Batch(batch) => {
                batch.validate().map_err(|_| PeerError::InvalidResponse)?
            }
            _ => {}
        }
        let cache_key = catalogue_cache_key(&import.summary, user, &request)?;
        let local = state
            .store
            .sharing_identity(clock_ms())
            .await
            .map_err(|_| PeerError::Unavailable)?;
        let credentials = ImportCredential::open(self, local.server_id, &import)
            .map_err(|_| PeerError::Unavailable)?;
        let expected = SharingIdentity {
            server_id: import.summary.source_server_id,
            catalogue_epoch: import.summary.catalogue_epoch,
            created_at_ms: 0,
        };
        let (mut peer, _) =
            PeerConnection::verified(self, &import.summary.endpoints, &expected).await?;
        self.ensure_current(state, &import.summary).await?;
        let mut reply = match request {
            CatalogueRead::Libraries => CatalogueReply::Libraries(
                peer.catalogue_libraries(&credentials.credential)
                    .await?
                    .libraries,
            ),
            CatalogueRead::Page {
                library,
                parent,
                q,
                cursor,
                limit,
            } => CatalogueReply::Page(
                peer.catalogue_page(
                    &credentials.credential,
                    &library,
                    parent.as_ref(),
                    &q,
                    cursor.as_deref(),
                    limit,
                )
                .await?,
            ),
            CatalogueRead::Item(item) => {
                CatalogueReply::Item(peer.catalogue_item(&credentials.credential, &item).await?)
            }
            CatalogueRead::Batch(batch) => CatalogueReply::Batch(
                peer.catalogue_batch(&credentials.credential, &batch)
                    .await?,
            ),
        };
        let current = state
            .store
            .sharing_import(import_id)
            .await
            .map_err(|_| PeerError::Unavailable)?
            .ok_or(PeerError::Unavailable)?;
        if current.summary.assignment_generation != import.summary.assignment_generation
            || current.summary.lifecycle_generation != import.summary.lifecycle_generation
            || current.summary.endpoint_generation != import.summary.endpoint_generation
            || current.summary.source_server_id != import.summary.source_server_id
            || current.summary.catalogue_epoch != import.summary.catalogue_epoch
            || current.summary.remote_grant_id != import.summary.remote_grant_id
            || current.summary.state != "active"
        {
            return Err(PeerError::Unavailable);
        }
        self.ensure_current(state, &import.summary).await?;
        let assigned = tokio::time::timeout(
            Duration::from_secs(1),
            state.store.assigned_catalogue_libraries(
                import_id,
                user,
                import.summary.lifecycle_generation,
                import.summary.assignment_generation,
            ),
        )
        .await
        .map_err(|_| PeerError::Unavailable)?
        .map_err(|_| PeerError::Unavailable)?;
        if assigned.is_empty() {
            return Err(PeerError::Authentication);
        }
        match &mut reply {
            CatalogueReply::Item(details) => {
                if !assigned.contains(&details.item.library_id) {
                    return Err(PeerError::Authentication);
                }
            }
            CatalogueReply::Libraries(libraries) => {
                libraries.retain(|library| assigned.contains(&library.library_id))
            }
            CatalogueReply::Page(page) => {
                if page
                    .items
                    .iter()
                    .any(|item| !assigned.contains(&item.library_id))
                {
                    return Err(PeerError::Authentication);
                }
            }
            CatalogueReply::Batch(batch) => {
                for entry in &mut batch.items {
                    if entry
                        .item
                        .as_ref()
                        .is_some_and(|item| !assigned.contains(&item.library_id))
                    {
                        entry.item = None;
                    }
                }
            }
        }
        let art_items: Vec<&plurx_core::sharing_catalogue::SourceCatalogueItem> = match &reply {
            CatalogueReply::Libraries(_) => vec![],
            CatalogueReply::Item(details) => vec![&details.item],
            CatalogueReply::Page(page) => page.items.iter().collect(),
            CatalogueReply::Batch(batch) => batch
                .items
                .iter()
                .filter_map(|entry| entry.item.as_ref())
                .collect(),
        };
        for item in art_items {
            for art in &item.art {
                let resource = art
                    .resource
                    .reference_unverified()
                    .map_err(|_| PeerError::InvalidResponse)?;
                if resource.server_id != import.summary.source_server_id
                    || resource.catalogue_epoch != import.summary.catalogue_epoch
                    || Some(resource.grant_id) != import.summary.remote_grant_id
                    || resource.expires_at_ms <= clock_ms()
                    || resource.expires_at_ms
                        > clock_ms()
                            .saturating_add(plurx_core::sharing_artwork::ART_RESOURCE_LIFETIME_MS)
                {
                    return Err(PeerError::InvalidResponse);
                }
            }
        }
        // Until the Source exposes a smaller current-digest proof, every hit
        // still takes the full fresh pinned read above. Changed or unauthorized
        // data can therefore never be served from this cache.
        let bytes = serde_json::to_vec(&reply).map_err(|_| PeerError::InvalidResponse)?;
        if bytes.len() > 4 * 1024 * 1024 {
            return Err(PeerError::InvalidResponse);
        }
        // Closed response bytes include the received revisions and observed
        // scope/catalogue counters. A different current witness gets a distinct
        // entry, even when the request and receiver generations are unchanged.
        use sha2::{Digest, Sha256};
        let cache_key: [u8; 32] = Sha256::new()
            .chain_update(cache_key)
            .chain_update(Sha256::digest(&bytes))
            .finalize()
            .into();
        let now = std::time::Instant::now();
        let mut cache = self.catalogue_cache.lock().expect("catalogue cache");
        if let Some(previous) = cache.fresh(&cache_key, now) {
            if previous.as_ref() == bytes.as_slice() {
                reply =
                    serde_json::from_slice(&previous).map_err(|_| PeerError::InvalidResponse)?;
            } else {
                cache.remember(cache_key, bytes, now);
            }
        } else {
            cache.remember(cache_key, bytes, now);
        }
        Ok((import.summary, reply))
    }
}

#[cfg(test)]
mod catalogue_admission_tests {
    use super::*;
    #[test]
    fn sharing_catalogue_admission_is_bounded_and_releases_on_cancellation() {
        let admission = Arc::new(CatalogueAdmission::default());
        let ids: Vec<_> = (0..5).map(|_| uuid::Uuid::new_v4()).collect();
        let mut permits: Vec<_> = ids[..4]
            .iter()
            .map(|id| admission.acquire(*id).expect("synthetic catalogue fixture"))
            .collect();
        assert!(admission.acquire(ids[0]).is_err());
        assert!(admission.acquire(ids[4]).is_err());
        drop(permits.pop());
        let replacement = admission
            .acquire(ids[4])
            .expect("synthetic catalogue fixture");
        drop(replacement);
        drop(permits);
        assert_eq!(admission.global.available_permits(), 4);
        assert!(admission
            .imports
            .lock()
            .expect("synthetic catalogue fixture")
            .is_empty());
        drop(
            admission
                .acquire(ids[0])
                .expect("synthetic catalogue fixture"),
        );
    }
}

#[cfg(test)]
mod catalogue_cancellation_tests {
    use super::*;
    #[tokio::test]
    async fn sharing_catalogue_aborted_operation_releases_import_and_global_admission() {
        let admission = Arc::new(CatalogueAdmission::default());
        let import = uuid::Uuid::new_v4();
        let task_admission = admission.clone();
        let (ready, waiting) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _permit = task_admission.acquire(import).expect("synthetic admission");
            ready.send(()).expect("fixture receiver");
            std::future::pending::<()>().await;
        });
        waiting.await.expect("fixture admission");
        assert!(admission.acquire(import).is_err());
        task.abort();
        assert!(task.await.expect_err("cancelled fixture").is_cancelled());
        assert_eq!(admission.global.available_permits(), 4);
        drop(admission.acquire(import).expect("released import"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plurx_core::{
        secrets::{CredentialKey, SharingSecretPurpose},
        sharing::{ImportSummary, StoredImport},
    };
    #[test]
    fn sharing_source_dispatch_retains_complete_receiver_recipe_and_private_request() {
        use plurx_core::{
            sharing::SourceId,
            sharing_catalogue::SharedReference,
            sharing_catalogue_details::FileRevision,
            sharing_receiver_sessions::{
                ReceiverProducerKind, ReceiverSessionIntent, RemoteSourceRecipe,
            },
            store::sharing_catalogue::ReceiverCatalogueScope,
        };
        let reference = SharedReference {
            import_id: uuid::Uuid::new_v4(),
            server_id: uuid::Uuid::new_v4(),
            catalogue_epoch: uuid::Uuid::new_v4(),
            library_id: SourceId::parse("0").expect("receiver request fixture"),
            item_id: SourceId::parse("9007199254740993").expect("receiver request fixture"),
        };
        let original = serde_json::json!({"playback_id":"player", "request_id":"client-attempt",
            "start":30.5,"caps":{"v":2,"nested":{"codec":"original"}},"audio":2});
        let intent = ReceiverSessionIntent {
            scope: ReceiverCatalogueScope {
                import_id: reference.import_id,
                source_server_id: reference.server_id,
                catalogue_epoch: reference.catalogue_epoch,
                lifecycle_generation: 3,
                assignment_generation: 4,
                endpoint_generation: 5,
                claim_id: uuid::Uuid::new_v4(),
                remote_grant_id: uuid::Uuid::new_v4(),
                libraries: vec![reference.library_id.clone()],
            },
            user_id: 1,
            login_hash: "a".repeat(64),
            source_position_ms: 0,
            recipe: RemoteSourceRecipe {
                kind: ReceiverProducerKind::RemoteSource,
                version: 1,
                reference,
                lifecycle_generation: 3,
                file_id: SourceId::parse("0").expect("receiver request fixture"),
                file_revision: FileRevision::parse(&"b".repeat(64))
                    .expect("receiver request fixture"),
                source_request_id: uuid::Uuid::new_v4(),
                parent_login_hash: "a".repeat(64),
                request_json: serde_json::to_string(&original).expect("receiver request fixture"),
            },
        };
        let recipe = &intent.recipe;
        let target = crate::http::hls::SourcePlaybackTarget {
            server_id: recipe.reference.server_id,
            catalogue_epoch: recipe.reference.catalogue_epoch,
            library_id: recipe.reference.library_id.clone(),
            item_id: recipe.reference.item_id.clone(),
            file_id: recipe.file_id.clone(),
            revision: recipe.file_revision.clone(),
        };
        let mut private = original.clone();
        private["request_id"] = recipe.source_request_id.to_string().into();
        let mut wrapper = serde_json::json!({"reference":target,"session":private});
        let encode = |value: &serde_json::Value| {
            serde_json::to_string(value).expect("receiver request fixture")
        };
        assert!(receiver_source_request(&intent, &encode(&wrapper)).is_ok());
        assert_eq!(recipe.request_json, encode(&original));
        for field in ["start", "audio", "caps"] {
            let saved = wrapper["session"][field].clone();
            wrapper["session"][field] = serde_json::Value::Null;
            assert!(
                receiver_source_request(&intent, &encode(&wrapper)).is_err(),
                "{field}"
            );
            wrapper["session"][field] = saved;
        }
        wrapper["session"]["request_id"] = uuid::Uuid::new_v4().to_string().into();
        assert!(receiver_source_request(&intent, &encode(&wrapper)).is_err());
        let mut different_login = intent.clone();
        different_login.login_hash = "c".repeat(64);
        wrapper["session"]["request_id"] = recipe.source_request_id.to_string().into();
        assert!(receiver_source_request(&different_login, &encode(&wrapper)).is_err());
    }
    #[test]
    fn sharing_import_ciphertext_preserves_pairing_metadata_without_the_bootstrap_secret() {
        let key = Arc::new(CredentialKey::from_bytes([41; 32]));
        let local = uuid::Uuid::new_v4();
        let import_id = uuid::Uuid::new_v4();
        let invitation = uuid::Uuid::new_v4();
        let grant = plurx_core::sharing::new_secret().expect("synthetic grant");
        let clear = ImportCredential::encode(
            &grant,
            invitation,
            1000,
            "Original recipient name",
            2000,
            Some(1800),
        )
        .expect("payload");
        let credential = key
            .seal_sharing(
                SharingSecretPurpose::Credential,
                local,
                import_id,
                clear.expose(),
            )
            .expect("sealed credential");
        assert!(!credential
            .to_persist()
            .expect("ciphertext")
            .contains(grant.expose()));
        assert!(!credential
            .to_persist()
            .expect("ciphertext")
            .contains("Original recipient name"));
        let mut stored = StoredImport {
            summary: ImportSummary {
                id: import_id,
                source_server_id: uuid::Uuid::new_v4(),
                catalogue_epoch: uuid::Uuid::new_v4(),
                source_name: "Source".into(),
                claim_id: uuid::Uuid::new_v4(),
                remote_grant_id: Some(uuid::Uuid::new_v4()),
                state: "pending".into(),
                assignment_generation: 1,
                lifecycle_generation: 1,
                endpoint_generation: 1,
                observed_endpoint_revision: None,
                endpoints: vec![],
            },
            credential,
            claim: None,
        };
        let manager = SharingManager::new(
            key,
            "unused fixture directory".into(),
            SharingNetworkConfig::default(),
        );
        let recovered = ImportCredential::open(&manager, local, &stored).expect("restart recovery");
        assert_eq!(recovered.credential.expose(), grant.expose());
        assert_eq!(recovered.invitation_id, invitation);
        assert_eq!(recovered.started_at_ms, 1000);
        assert_eq!(recovered.claim_deadline_ms, 2000);
        assert_eq!(recovered.pending_expires_at_ms, Some(1800));
        assert_eq!(recovered.recipient_name, "Original recipient name");
        assert!(ImportCredential::open(&manager, uuid::Uuid::new_v4(), &stored).is_err());
        stored.summary.id = uuid::Uuid::new_v4();
        assert!(ImportCredential::open(&manager, local, &stored).is_err());
    }
}

#[cfg(test)]
mod catalogue_cache_tests {
    use super::*;
    fn key(n: u64) -> [u8; 32] {
        let mut key = [0; 32];
        key[..8].copy_from_slice(&n.to_be_bytes());
        key
    }
    #[test]
    fn sharing_catalogue_cache_enforces_ram_lru_entry_and_absolute_ttl_bounds() {
        let now = std::time::Instant::now();
        let mut cache = CatalogueCache::default();
        for n in 0..8 {
            cache.remember(key(n), vec![n as u8; 4 * 1024 * 1024], now);
        }
        assert!(cache.bytes <= CATALOGUE_CACHE_BYTES);
        assert!(cache.fresh(&key(0), now).is_none());
        assert!(cache.fresh(&key(1), now).is_some());
        cache.remember(key(8), vec![8; 4 * 1024 * 1024], now);
        assert!(cache.fresh(&key(2), now).is_none());
        assert!(cache.fresh(&key(1), now).is_some());
        // Touching never extends the insertion TTL.
        assert!(cache
            .fresh(&key(1), now + Duration::from_secs(29))
            .is_some());
        assert!(cache
            .fresh(&key(1), now + Duration::from_secs(30))
            .is_none());
        cache.remember(key(9), vec![0; 4 * 1024 * 1024 + 1], now);
        assert!(!cache.entries.contains_key(&key(9)));
        cache = CatalogueCache::default();
        for n in 0..2049 {
            cache.remember(key(n), vec![1], now);
        }
        assert_eq!(cache.entries.len(), 2048);
        assert!(!cache.entries.contains_key(&key(0)));
        assert_eq!(cache.bytes, 2048 * 257);
        cache.remember(key(2048), vec![2, 3], now);
        assert_eq!(
            cache.fresh(&key(2048), now).expect("replacement").as_ref(),
            &[2, 3]
        );
        cache.tick = u64::MAX;
        assert!(cache.fresh(&key(2048), now).is_none());
        assert!(cache.entries.is_empty());
        assert_eq!(cache.bytes, 0);
    }
    #[test]
    fn sharing_catalogue_cache_key_separates_full_authority_and_request() {
        let summary = plurx_core::sharing::ImportSummary {
            id: uuid::Uuid::new_v4(),
            source_server_id: uuid::Uuid::new_v4(),
            catalogue_epoch: uuid::Uuid::new_v4(),
            source_name: "Fixture".into(),
            claim_id: uuid::Uuid::new_v4(),
            remote_grant_id: Some(uuid::Uuid::new_v4()),
            state: "active".into(),
            assignment_generation: 1,
            lifecycle_generation: 1,
            endpoint_generation: 1,
            observed_endpoint_revision: None,
            endpoints: Vec::new(),
        };
        let request = CatalogueRead::Item(
            plurx_core::sharing::SourceId::parse("9007199254740993").expect("large ID"),
        );
        let key = catalogue_cache_key(&summary, 1, &request).expect("key");
        assert_ne!(
            key,
            catalogue_cache_key(&summary, 2, &request).expect("other user")
        );
        assert_ne!(
            key,
            catalogue_cache_key(&summary, 1, &CatalogueRead::Libraries).expect("other operation")
        );
        for field in 0..8 {
            let mut other = summary.clone();
            match field {
                0 => other.id = uuid::Uuid::new_v4(),
                1 => other.source_server_id = uuid::Uuid::new_v4(),
                2 => other.catalogue_epoch = uuid::Uuid::new_v4(),
                3 => other.claim_id = uuid::Uuid::new_v4(),
                4 => other.remote_grant_id = Some(uuid::Uuid::new_v4()),
                5 => other.lifecycle_generation += 1,
                6 => other.assignment_generation += 1,
                _ => other.endpoint_generation += 1,
            };
            assert_ne!(
                key,
                catalogue_cache_key(&other, 1, &request).expect("changed authority")
            );
        }
        let page = |q: &str, cursor: Option<String>| CatalogueRead::Page {
            library: plurx_core::sharing::SourceId::parse("1").expect("ID"),
            parent: None,
            q: q.into(),
            cursor,
            limit: 200,
        };
        assert_ne!(
            catalogue_cache_key(&summary, 1, &page("a", None)).expect("filter"),
            catalogue_cache_key(&summary, 1, &page("b", None)).expect("other filter")
        );
        assert_ne!(
            catalogue_cache_key(&summary, 1, &page("a", None)).expect("filter"),
            catalogue_cache_key(&summary, 1, &page("a", Some("opaque".into()))).expect("cursor")
        );
    }
    #[tokio::test]
    async fn sharing_scope_control_capacity_is_independent_and_cancellation_releases() {
        let directory = tempfile::tempdir().expect("control fixture");
        let manager = SharingManager::new(
            Arc::new(CredentialKey::from_bytes([41; 32])),
            directory.path().to_path_buf(),
            Default::default(),
        );
        let catalogue = manager.catalogue_admission.clone();
        let busy = catalogue.acquire(uuid::Uuid::new_v4()).expect("catalogue");
        let controls = manager.scope_control.clone();
        let full = controls
            .clone()
            .try_acquire_many_owned(32)
            .expect("control cap");
        assert!(controls.clone().try_acquire_owned().is_err());
        assert_eq!(catalogue.global.available_permits(), 3);
        drop(full);
        drop(busy);
        let cloned = controls.clone();
        let (started, ready) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _permit = cloned.try_acquire_owned().expect("control");
            started.send(()).expect("ready");
            std::future::pending::<()>().await;
        });
        ready.await.expect("admitted");
        assert_eq!(controls.available_permits(), 31);
        task.abort();
        let _ = task.await;
        assert_eq!(controls.available_permits(), 32);
    }
}

#[cfg(test)]
mod loop_pacing_tests {
    use super::*;
    #[test]
    fn sharing_claim_schedule_refreshes_healthy_active_imports_at_the_backoff_ceiling() {
        assert_eq!(claim_retry_delay(0, true), ACTIVE_IMPORT_REFRESH);
        assert_eq!(claim_retry_delay(0, false), Duration::from_secs(5));
        for (attempts, base) in [(1, 10), (2, 20), (3, 40), (4, 60), (9, 60)] {
            for active in [false, true] {
                let delay = claim_retry_delay(attempts, active);
                assert!(
                    delay >= Duration::from_secs(base) && delay < Duration::from_secs(base + 1),
                    "attempt {attempts}: {delay:?}"
                );
            }
        }
    }
    #[tokio::test(start_paused = true)]
    async fn sharing_local_enable_wakes_idle_loops_before_their_remote_polls() {
        let directory = tempfile::tempdir().expect("wake fixture");
        let manager = SharingManager::new(
            Arc::new(CredentialKey::from_bytes([41; 32])),
            directory.path().to_path_buf(),
            Default::default(),
        );
        let shutdown = CancellationToken::new();
        let within = |started: tokio::time::Instant, bound: Duration| {
            let elapsed = started.elapsed();
            elapsed >= bound && elapsed < bound + Duration::from_millis(10)
        };
        // Written before either loop waits: the permit is kept, not lost.
        let started = tokio::time::Instant::now();
        manager.enablement_written(true);
        assert!(manager.listener_idle(&shutdown).await);
        assert!(manager.claims_idle(None, &shutdown).await);
        assert_eq!(started.elapsed(), Duration::ZERO);
        // Written while the claim loop sleeps.
        let started = tokio::time::Instant::now();
        let (woken, ()) = tokio::join!(manager.claims_idle(None, &shutdown), async {
            tokio::task::yield_now().await;
            manager.enablement_written(true);
        });
        assert!(woken);
        assert!(manager.listener_idle(&shutdown).await);
        assert_eq!(started.elapsed(), Duration::ZERO);
        // A disable cancels bodies and wakes nothing; only the polls end the
        // wait, as they do for a write made on another node.
        manager.enablement_written(false);
        let started = tokio::time::Instant::now();
        assert!(manager.listener_idle(&shutdown).await);
        assert!(within(started, LISTENER_IDLE_POLL));
        let started = tokio::time::Instant::now();
        assert!(manager.claims_idle(None, &shutdown).await);
        assert!(within(started, CLAIM_IDLE_POLL));
        // A known due time sooner than the poll ends the sleep then, and one
        // past it never stretches the poll.
        let started = tokio::time::Instant::now();
        assert!(
            manager
                .claims_idle(Some(started + Duration::from_secs(3)), &shutdown)
                .await
        );
        assert!(within(started, Duration::from_secs(3)));
        let started = tokio::time::Instant::now();
        assert!(
            manager
                .claims_idle(Some(started + ACTIVE_IMPORT_REFRESH), &shutdown)
                .await
        );
        assert!(within(started, CLAIM_IDLE_POLL));
        shutdown.cancel();
        assert!(!manager.listener_idle(&shutdown).await);
        assert!(!manager.claims_idle(None, &shutdown).await);
    }
}
