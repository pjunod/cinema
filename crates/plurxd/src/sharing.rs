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
}
pub(crate) struct SharingManager {
    pub key: Arc<CredentialKey>,
    pub network: SharingNetworkConfig,
    key_directory: PathBuf,
    status: RwLock<SharingStatus>,
    lifetime: Mutex<Option<CancellationToken>>,
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
    pub fn new(
        key: Arc<CredentialKey>,
        key_directory: PathBuf,
        network: SharingNetworkConfig,
    ) -> Self {
        Self {
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
            }),
            network,
            lifetime: Mutex::new(None),
        }
    }
    pub fn status(&self) -> SharingStatus {
        self.status.read().expect("sharing status lock").clone()
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
            tokio::select! { () = shutdown.cancelled() => { self.disable_bodies(); return; }, () = tokio::time::sleep(Duration::from_secs(5)) => {} }
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
    pub async fn resume_import(
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
        let mut retry = std::collections::HashMap::<uuid::Uuid, (u32, tokio::time::Instant)>::new();
        loop {
            if shutdown.is_cancelled() {
                return;
            }
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
                    retry.retain(|id, _| {
                        imports.iter().any(|import| {
                            import.id == *id
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
                            .is_some_and(|(_, due)| *due > tokio::time::Instant::now())
                        {
                            continue;
                        }
                        let id = summary.id;
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
                                Ok(Some(import)) => manager.resume_import(state, import).await,
                                _ => Err(crate::sharing_client::PeerError::Unavailable),
                            };
                            (id, result, observed)
                        });
                    }
                    loop {
                        tokio::select! {
                            ()=shutdown.cancelled()=>return,
                            completed=requests.next()=>match completed {
                                Some((id,result,observed))=> {
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
                                    let attempts=if result.is_ok(){0}else{retry.get(&id).map_or(1,|(attempts,_)|attempts.saturating_add(1))};
                                    let seconds=(5u64.saturating_mul(1u64<<attempts.min(4))).min(60);
                                    let jitter = if attempts==0 {0}else{uuid::Uuid::new_v4().as_u128() as u64 % 1000};
                                    retry.insert(id,(attempts,tokio::time::Instant::now()+Duration::from_millis(seconds*1000+jitter)));
                                },
                                None=>break,
                            }
                        }
                    }
                }
            }
            tokio::select! {()=shutdown.cancelled()=>return,()=tokio::time::sleep(Duration::from_secs(1))=>{}}
        }
    }
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
            .inspect_err(|_| {
                tracing::warn!(phase = "verify_peer", "sharing rotation failed");
            })?;
        self.ensure_current(state, &import.summary)
            .await
            .inspect_err(|_| {
                tracing::warn!(phase = "current_import", "sharing rotation failed");
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
                .inspect_err(|_| {
                    tracing::warn!(phase = "peer_swap", "sharing rotation failed");
                })?;
        }
        let confirmed: GrantResponse = peer
            .grant(&replacement_credential.credential)
            .await
            .inspect_err(|_| {
                tracing::warn!(phase = "confirm_replacement", "sharing rotation failed");
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

#[cfg(test)]
mod tests {
    use super::*;
    use plurx_core::{
        secrets::{CredentialKey, SharingSecretPurpose},
        sharing::{ImportSummary, StoredImport},
    };
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
