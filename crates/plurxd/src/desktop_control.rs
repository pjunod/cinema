//! Peer-bound native lifecycle IPC. Never mounted on the TCP router.
//! The companion owns supervisors and installation; this module owns only
//! admissions, the exact running process and its durable shutdown evidence.
#[cfg(target_os = "macos")]
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{ensure, Context};
use axum::{
    extract::{DefaultBodyLimit, Extension, FromRequestParts, State},
    http::{Request, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use hyper_util::{rt::TokioIo, service::TowerToHyperService};
use plurx_core::cluster::{
    membership::MembershipStatus,
    migration::status::{ReplicationBackend, ReplicationHealth},
};
use plurx_desktop_control::protocol::{
    DaemonIdentityV1, ExitClass, Operation, ProcessIdentityV1, ShutdownCommitRequestV1,
    ShutdownCommittedV1, ShutdownPrepareRequestV1, ShutdownPreparedV1, ShutdownReceiptV1,
    ShutdownStatusV1, SupervisorIdentityV1, SupervisorKind,
};
use sha2::{Digest, Sha256};
use tokio::net::{UnixListener, UnixStream};
use tokio_util::sync::CancellationToken;

use crate::{
    http::cluster_operations::{self, PlannedOutageLeaseGuard, PlannedOutageOperation},
    serving_fence::PlannedOutageFenceToken,
    state::AppState,
};

const PREPARATION_MS: u64 = 60_000;
const IPC_DRAIN: Duration = Duration::from_secs(5);

#[derive(Clone)]
struct Peer {
    uid: u32,
    pid: u32,
    generation: String,
    connection_id: String,
}
impl Peer {
    fn from_socket(socket: &UnixStream) -> anyhow::Result<Self> {
        let cred = socket.peer_cred()?;
        #[cfg(target_os = "linux")]
        let pid = cred
            .pid()
            .and_then(|pid| u32::try_from(pid).ok())
            .context("missing peer PID")?;
        #[cfg(target_os = "macos")]
        let pid = {
            let mut pid: libc::pid_t = 0;
            let mut len = std::mem::size_of_val(&pid) as libc::socklen_t;
            // SAFETY: valid connected socket, initialized fixed-size PID buffer.
            let result = unsafe {
                libc::getsockopt(
                    socket.as_raw_fd(),
                    libc::SOL_LOCAL,
                    libc::LOCAL_PEERPID,
                    (&mut pid as *mut libc::pid_t).cast(),
                    &mut len,
                )
            };
            ensure!(result == 0 && pid > 0, "missing local peer PID");
            pid as u32
        };
        Ok(Self {
            uid: cred.uid(),
            pid,
            // A restricted /proc may hide an unrelated desktop/root-helper
            // process. Kernel credentials still authorize this held socket;
            // an unguessable connection identity forbids preparation transfer
            // even when the optional client start generation cannot be read.
            generation: process_generation(pid).unwrap_or_default(),
            connection_id: random_capability(),
        })
    }
    fn current(&self) -> bool {
        self.generation.is_empty()
            || process_generation(self.pid).is_ok_and(|generation| generation == self.generation)
    }
    fn native_owner(&self, owner: u32) -> bool {
        #[cfg(target_os = "linux")]
        {
            let _ = owner;
            self.uid == 0
        }
        #[cfg(target_os = "macos")]
        {
            self.uid == owner
        }
    }
}

pub(crate) struct DesktopControl {
    installation_id: String,
    socket: PathBuf,
    receipts: PathBuf,
    process: ProcessIdentityV1,
    supervisor: SupervisorIdentityV1,
    operation: tokio::sync::Mutex<Option<Preparation>>,
    http_drained: AtomicBool,
    auxiliary_drained: AtomicBool,
}
struct Preparation {
    operation_id: String,
    generation: String,
    capability: String,
    expires: u64,
    token: PlannedOutageFenceToken,
    lease: Option<PlannedOutageLeaseGuard>,
    committed: bool,
    // A preparation can only be exercised by its original OS process. Saved
    // admin tokens or a disclosed capability alone do not transfer ownership.
    peer: Peer,
}

impl DesktopControl {
    pub(crate) fn from_environment() -> anyhow::Result<Option<Arc<Self>>> {
        let values = [
            "PLURX_DESKTOP_INSTALLATION_ID",
            "PLURX_DESKTOP_CONTROL_SOCKET",
            "PLURX_DESKTOP_RECEIPT_DIR",
        ]
        .map(std::env::var_os);
        if values.iter().all(Option::is_none) {
            return Ok(None);
        }
        let [id, socket, receipts] = values;
        let installation_id = id
            .context("missing desktop installation ID")?
            .into_string()
            .map_err(|_| anyhow::anyhow!("invalid installation ID"))?;
        uuid::Uuid::parse_str(&installation_id).context("invalid installation UUID")?;
        let socket = PathBuf::from(socket.context("missing desktop control socket")?);
        let receipts = PathBuf::from(receipts.context("missing desktop receipt directory")?);
        // SAFETY: geteuid has no preconditions and does not change process state.
        let uid = unsafe { libc::geteuid() };
        #[cfg(target_os = "linux")]
        {
            ensure!(
                uid != 0,
                "managed server must use its dedicated service user"
            );
            ensure!(
                socket == Path::new("/run/plurx/desktop-control.sock")
                    && receipts == Path::new("/var/lib/plurx-control/shutdown-receipts"),
                "unexpected managed control paths"
            );
        }
        #[cfg(target_os = "macos")]
        {
            let root = PathBuf::from(std::env::var_os("HOME").context("missing native home")?)
                .join("Library/Application Support/plurx/runtime/control");
            ensure!(
                socket == root.join("desktop-control.sock")
                    && receipts == root.join("shutdown-receipts"),
                "unexpected native control paths"
            );
        }
        validate_directory(socket.parent().context("missing socket parent")?, uid)?;
        validate_directory(&receipts, uid)?;
        let start_generation = process_generation(std::process::id())?;
        #[cfg(target_os = "linux")]
        let supervisor = SupervisorIdentityV1 {
            kind: SupervisorKind::Systemd,
            name: "plurxd.service".into(),
            invocation_id: {
                let invocation = std::env::var("INVOCATION_ID")
                    .context("managed service lacks systemd invocation ID")?;
                ensure!(
                    invocation.len() == 32 && invocation.bytes().all(|b| b.is_ascii_hexdigit()),
                    "invalid systemd invocation ID"
                );
                invocation
            },
        };
        #[cfg(target_os = "macos")]
        let supervisor = SupervisorIdentityV1 {
            kind: SupervisorKind::Launchd,
            name: "com.plurx.plurxd".into(),
            invocation_id: start_generation.clone(),
        };
        Ok(Some(Arc::new(Self {
            installation_id,
            socket,
            receipts,
            process: ProcessIdentityV1 {
                pid: std::process::id(),
                uid,
                start_generation,
                nonce: random_capability(),
            },
            supervisor,
            operation: tokio::sync::Mutex::new(None),
            http_drained: AtomicBool::new(false),
            auxiliary_drained: AtomicBool::new(true),
        })))
    }

    pub(crate) async fn is_committed(&self) -> bool {
        self.operation
            .lock()
            .await
            .as_ref()
            .is_some_and(|operation| operation.committed)
    }

    pub(crate) fn record_http_drain(&self, clean: bool) {
        self.http_drained.store(clean, Ordering::Release);
    }
    pub(crate) fn record_auxiliary_failure(&self) {
        self.auxiliary_drained.store(false, Ordering::Release);
    }

    /// Called after the selected store has closed, before run() can return.
    /// An interrupted/failed fsync propagates failure, never a clean receipt.
    pub(crate) async fn finish(&self, store_closed: bool) -> anyhow::Result<()> {
        let operation = self.operation.lock().await;
        let Some(prepared) = operation.as_ref().filter(|prepared| prepared.committed) else {
            return Ok(());
        };
        let http_drained = self.http_drained.load(Ordering::Acquire);
        let auxiliary_drained = self.auxiliary_drained.load(Ordering::Acquire);
        let exit_class = if auxiliary_drained {
            classify_exit(http_drained, store_closed)
        } else {
            ExitClass::Unclean
        };
        let receipt = ShutdownReceiptV1 {
            schema_version: 1,
            operation_id: prepared.operation_id.clone(),
            process: self.process.clone(),
            generation: prepared.generation.clone(),
            http_drained,
            store_closed,
            exit_class,
        };
        write_receipt(&self.receipts, &receipt)?;
        ensure!(
            http_drained && store_closed && auxiliary_drained,
            "native shutdown did not drain and close cleanly"
        );
        Ok(())
    }

    pub(crate) async fn serve(
        self: &Arc<Self>,
        state: AppState,
        browser_origin: String,
    ) -> anyhow::Result<DesktopServer> {
        validate_directory(
            self.socket.parent().context("missing socket parent")?,
            self.process.uid,
        )?;
        if let Ok(metadata) = std::fs::symlink_metadata(&self.socket) {
            use std::os::unix::fs::FileTypeExt;
            ensure!(
                metadata.file_type().is_socket() && metadata.uid() == self.process.uid,
                "unexpected control socket owner/type"
            );
            ensure!(
                UnixStream::connect(&self.socket).await.is_err(),
                "another native controller listener is running"
            );
            std::fs::remove_file(&self.socket)?;
        }
        let listener = UnixListener::bind(&self.socket).context("bind native control socket")?;
        #[cfg(target_os = "linux")]
        let mode = 0o666;
        #[cfg(target_os = "macos")]
        let mode = 0o600;
        std::fs::set_permissions(&self.socket, std::fs::Permissions::from_mode(mode))?;
        let daemon_identity = DaemonIdentityV1 {
            schema_version: 1,
            installation_id: self.installation_id.clone(),
            process: self.process.clone(),
            node_id: state.node_id.clone(),
            instance_id: state.store.instance_id().await?,
            version: crate::version::SEMVER.into(),
            build: crate::version::SOURCE_COMMIT.into(),
            browser_origin,
            supervisor: self.supervisor.clone(),
            ready: state.serving.is_ready(),
        };
        let context = Arc::new(ContextState {
            control: Arc::clone(self),
            identity: daemon_identity,
        });
        let mutations = Router::new()
            .route("/local/v1/shutdown/prepare", post(prepare))
            .route("/local/v1/shutdown/commit", post(commit))
            .route("/local/v1/shutdown/cancel", post(cancel))
            .route_layer(middleware::from_fn_with_state(state.clone(), authorize));
        let app = Router::new()
            .merge(mutations)
            .route("/local/v1/identity", get(identity))
            .route("/local/v1/shutdown/status", get(status))
            .merge(crate::http::desktop_auth_router())
            .layer(Extension(context))
            .layer(DefaultBodyLimit::max(16 * 1024))
            .with_state(state);
        let stop = CancellationToken::new();
        let stopping = stop.clone();
        let path = self.socket.clone();
        let task = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    biased;
                    () = stopping.cancelled() => break,
                    accepted = listener.accept() => {
                        let (socket, _) = accepted?;
                        if connections.len() >= 32 { continue; }
                        let Ok(peer) = Peer::from_socket(&socket) else { continue; };
                        let app = app.clone().layer(Extension(peer));
                        let stop_connection = stopping.clone();
                        connections.spawn(async move {
                            let connection = hyper::server::conn::http1::Builder::new()
                                .header_read_timeout(Duration::from_secs(10)).timer(hyper_util::rt::TokioTimer::new())
                                .serve_connection(TokioIo::new(socket), TowerToHyperService::new(app));
                            tokio::pin!(connection);
                            tokio::select! {
                                result = &mut connection => { let _ = result; }
                                () = stop_connection.cancelled() => {
                                    connection.as_mut().graceful_shutdown();
                                    let _ = tokio::time::timeout(IPC_DRAIN, &mut connection).await;
                                }
                            }
                        });
                    }
                    Some(_) = connections.join_next(), if !connections.is_empty() => {}
                }
            }
            while connections.join_next().await.is_some() {}
            std::fs::remove_file(path)?;
            Ok(())
        });
        Ok(DesktopServer { stop, task })
    }
}

pub(crate) struct DesktopServer {
    stop: CancellationToken,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}
impl DesktopServer {
    pub(crate) async fn finish(self) -> anyhow::Result<()> {
        self.stop.cancel();
        self.task.await??;
        Ok(())
    }
}
#[derive(Clone)]
struct ContextState {
    control: Arc<DesktopControl>,
    identity: DaemonIdentityV1,
}
type LocalResult<T> = Result<Json<T>, LocalError>;
struct LocalError(StatusCode, &'static str, &'static str);
impl IntoResponse for LocalError {
    fn into_response(self) -> Response {
        (
            self.0,
            Json(serde_json::json!({"code":self.1,"message":self.2,"retryable":false})),
        )
            .into_response()
    }
}
fn unsafe_shutdown(message: &'static str) -> LocalError {
    LocalError(StatusCode::CONFLICT, "unsafe_shutdown", message)
}
fn invalid(message: &'static str) -> LocalError {
    LocalError(StatusCode::BAD_REQUEST, "invalid_request", message)
}
fn binding(control: &DesktopControl, peer: &Peer, nonce: &str) -> Result<(), LocalError> {
    if !peer.current()
        || nonce != control.process.nonce
        || process_generation(control.process.pid).ok().as_ref()
            != Some(&control.process.start_generation)
    {
        return Err(LocalError(
            StatusCode::CONFLICT,
            "identity_mismatch",
            "the connected process generation changed; repeat peer verification",
        ));
    }
    Ok(())
}
async fn authorize(
    State(state): State<AppState>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let Some(peer) = request.extensions().get::<Peer>().cloned() else {
        return LocalError(
            StatusCode::FORBIDDEN,
            "authorization_denied",
            "missing kernel peer identity",
        )
        .into_response();
    };
    let Some(context) = request.extensions().get::<Arc<ContextState>>().cloned() else {
        return LocalError(
            StatusCode::FORBIDDEN,
            "authorization_denied",
            "missing local context",
        )
        .into_response();
    };
    if !peer.current() {
        return LocalError(
            StatusCode::CONFLICT,
            "identity_mismatch",
            "peer process generation changed",
        )
        .into_response();
    }
    if peer.native_owner(context.control.process.uid) {
        return next.run(request).await;
    }
    let (mut parts, body) = request.into_parts();
    if let Err(error) =
        crate::http::extract::AdminUser::from_request_parts(&mut parts, &state).await
    {
        return error.into_response();
    }
    next.run(Request::from_parts(parts, body)).await
}
async fn identity(
    State(state): State<AppState>,
    Extension(context): Extension<Arc<ContextState>>,
) -> Json<DaemonIdentityV1> {
    let mut identity = context.identity.clone();
    identity.ready = state.serving.is_ready();
    Json(identity)
}
async fn status(Extension(context): Extension<Arc<ContextState>>) -> LocalResult<ShutdownStatusV1> {
    let operation = context.control.operation.try_lock().map_err(|_| {
        LocalError(
            StatusCode::SERVICE_UNAVAILABLE,
            "busy",
            "a native lifecycle operation is being observed; retry shortly",
        )
    })?;
    let active = operation
        .as_ref()
        .filter(|prepared| prepared.committed || prepared.expires > unix_ms());
    Ok(Json(ShutdownStatusV1 {
        schema_version: 1,
        operation_id: active.map(|p| p.operation_id.clone()),
        phase: active
            .map_or(
                "idle",
                |p| if p.committed { "committed" } else { "prepared" },
            )
            .into(),
        generation: active.map(|p| p.generation.clone()),
    }))
}

async fn prepare(
    State(state): State<AppState>,
    Extension(context): Extension<Arc<ContextState>>,
    Extension(peer): Extension<Peer>,
    request: Result<Json<ShutdownPrepareRequestV1>, axum::extract::rejection::JsonRejection>,
) -> LocalResult<ShutdownPreparedV1> {
    let Json(request) = request.map_err(|_| invalid("invalid shutdown preparation JSON"))?;
    let operation_guard = state
        .serving
        .try_planned_outage_operation_guard()
        .map_err(|_| unsafe_shutdown("another planned outage operation is resolving"))?;
    tokio::spawn(async move {
        let _operation_guard = operation_guard;
        prepare_owned(state, context, peer, request).await
    })
    .await
    .map_err(|_| unsafe_shutdown("native preparation owner failed"))?
}
async fn prepare_owned(
    state: AppState,
    context: Arc<ContextState>,
    peer: Peer,
    request: ShutdownPrepareRequestV1,
) -> LocalResult<ShutdownPreparedV1> {
    let control = &context.control;
    binding(control, &peer, &request.process_nonce)?;
    if request.schema_version != 1
        || uuid::Uuid::parse_str(&request.operation_id).is_err()
        || !request.confirm_interruption
    {
        return Err(invalid(
            "operation UUID, protocol version and interruption confirmation are required",
        ));
    }
    if !matches!(
        request.operation,
        Operation::Stop | Operation::Restart | Operation::Upgrade
    ) || (request.operation == Operation::Upgrade
        && request
            .target_version
            .as_ref()
            .is_none_or(|v| v.is_empty() || v.len() > 80))
    {
        return Err(invalid("invalid shutdown operation or upgrade target"));
    }
    let mut operation = control.operation.lock().await;
    if operation
        .as_ref()
        .is_some_and(|p| p.committed || p.expires > unix_ms())
    {
        return Err(LocalError(
            StatusCode::CONFLICT,
            "busy",
            "a native shutdown operation already owns this process",
        ));
    }
    if let Some(old) = operation.take() {
        if let Some(lease) = old.lease {
            lease.release().await;
        } else {
            state
                .serving
                .cancel_planned_outage_preparation(old.token, 0)
                .await;
        }
    }
    current_topology(&state).await?;
    let active = cluster_operations::local_owned_media_sessions(&state).await;
    if !state.serving.restart_drain_status(active).await.drained {
        return Err(unsafe_shutdown(
            "playback, recordings or response delivery remain active; wait for idle and try again",
        ));
    }
    let uninitialized = state
        .store
        .count_users()
        .await
        .map_err(|_| unsafe_shutdown("cannot verify first-run setup state"))?
        == 0;
    if uninitialized && !peer.native_owner(control.process.uid) {
        return Err(LocalError(
            StatusCode::FORBIDDEN,
            "authorization_denied",
            "first-run shutdown requires verified native-owner authority",
        ));
    }
    let (mut lease, evidence) = cluster_operations::acquire_planned_outage_preflight(
        &state,
        &state.node_id,
        Duration::from_millis(PREPARATION_MS),
        PlannedOutageOperation::Restart,
    )
    .await
    .map_err(|_| unsafe_shutdown("cannot reserve the cluster planned-outage operation"))?;
    let membership = evidence.membership;
    if let Err(error) = topology_allowed(
        &membership,
        &state.node_id,
        evidence.verdict.safe_to_restart_one,
        evidence.verdict.candidate_node_id.as_deref(),
    ) {
        lease.release().await;
        return Err(error);
    }
    let expires = unix_ms().saturating_add(PREPARATION_MS).min(
        lease
            .claim()
            .preparation_expiry_unix_ms(Duration::from_millis(PREPARATION_MS))
            .unwrap_or(0),
    );
    let Some(token) = state.serving.begin_desktop_preparation_until(expires).await else {
        lease.release().await;
        return Err(unsafe_shutdown(
            "another local planned outage already owns admissions",
        ));
    };
    lease.arm_local_fence(token);
    if cluster_operations::local_owned_media_sessions(&state).await != 0 {
        lease.release().await;
        return Err(unsafe_shutdown(
            "playback or recordings remain active; wait for idle and try again",
        ));
    }
    if !matches!(
        tokio::time::timeout(
            Duration::from_secs(3),
            state
                .serving
                .wait_for_restart_admissions_until(token, &state.shutdown)
        )
        .await,
        Ok(true)
    ) {
        lease.release().await;
        return Err(unsafe_shutdown(
            "preparation expired before media admissions drained",
        ));
    }
    let active = cluster_operations::local_owned_media_sessions(&state).await;
    if active != 0 || !state.serving.planned_outage_is_current(token).await {
        lease.release().await;
        return Err(unsafe_shutdown(
            "playback or recordings remain active; wait for idle and try again",
        ));
    }
    let generation = topology_generation(&membership);
    let capability = random_capability();
    let response = ShutdownPreparedV1 {
        schema_version: 1,
        operation_id: request.operation_id.clone(),
        process_nonce: control.process.nonce.clone(),
        generation: generation.clone(),
        expires_unix_ms: expires,
        owner_capability: capability.clone(),
    };
    *operation = Some(Preparation {
        operation_id: request.operation_id,
        generation,
        capability,
        expires,
        token,
        lease: Some(lease),
        committed: false,
        peer,
    });
    Ok(Json(response))
}

async fn commit(
    State(state): State<AppState>,
    Extension(context): Extension<Arc<ContextState>>,
    Extension(peer): Extension<Peer>,
    request: Result<Json<ShutdownCommitRequestV1>, axum::extract::rejection::JsonRejection>,
) -> LocalResult<ShutdownCommittedV1> {
    let Json(request) = request.map_err(|_| invalid("invalid shutdown commitment JSON"))?;
    // Continue through a disconnected HTTP waiter: after atomic commitment,
    // cancellation belongs to the daemon, not to the controller connection.
    let operation_guard = state
        .serving
        .try_planned_outage_operation_guard()
        .map_err(|_| unsafe_shutdown("another planned outage operation is resolving"))?;
    tokio::spawn(async move {
        let _operation_guard = operation_guard;
        commit_owned(state, context, peer, request).await
    })
    .await
    .map_err(|_| unsafe_shutdown("native commitment owner failed"))?
}
async fn commit_owned(
    state: AppState,
    context: Arc<ContextState>,
    peer: Peer,
    request: ShutdownCommitRequestV1,
) -> LocalResult<ShutdownCommittedV1> {
    binding(&context.control, &peer, &request.process_nonce)?;
    let mut operation = context.control.operation.lock().await;
    let prepared = validate_preparation(&mut operation, &request, &peer)?;
    if prepared.committed {
        return Err(unsafe_shutdown("shutdown is already committed"));
    }
    // Reassert the exact original claim, retaining its original deadline.
    // The membership anti-replay receipt rejects a claim cancelled through
    // another administrator route; a token alone cannot outlive that lease.
    let lease = prepared
        .lease
        .as_ref()
        .ok_or_else(|| unsafe_shutdown("the replicated preparation owner is missing"))?;
    state
        .membership
        .commit_cluster_operation_lease(lease.claim())
        .await
        .map_err(|_| {
            unsafe_shutdown("the exact replicated preparation was cancelled or superseded")
        })?;
    current_topology(&state).await?;
    let evidence = cluster_operations::recheck_desktop_preparation(&state, prepared.token)
        .await
        .map_err(|_| unsafe_shutdown("fresh cluster verdict is unavailable"))?;
    let membership = evidence.membership;
    topology_allowed(
        &membership,
        &state.node_id,
        evidence.verdict.safe_to_restart_one,
        evidence.verdict.candidate_node_id.as_deref(),
    )?;
    if topology_generation(&membership) != prepared.generation {
        return Err(unsafe_shutdown(
            "topology or protocol generation changed; prepare again",
        ));
    }
    let active = cluster_operations::local_owned_media_sessions(&state).await;
    binding(&context.control, &peer, &request.process_nonce)?;
    if !state
        .serving
        .commit_desktop_shutdown(prepared.token, active)
        .await
    {
        return Err(unsafe_shutdown(
            "preparation expired or media ownership changed before commitment",
        ));
    }
    prepared.committed = true;
    if let Some(lease) = prepared.lease.take() {
        lease.disarm();
    }
    state.shutdown.cancel();
    Ok(Json(ShutdownCommittedV1 {
        schema_version: 1,
        operation_id: prepared.operation_id.clone(),
        process_nonce: context.control.process.nonce.clone(),
        generation: prepared.generation.clone(),
        committed: true,
    }))
}
async fn cancel(
    State(state): State<AppState>,
    Extension(context): Extension<Arc<ContextState>>,
    Extension(peer): Extension<Peer>,
    request: Result<Json<ShutdownCommitRequestV1>, axum::extract::rejection::JsonRejection>,
) -> LocalResult<serde_json::Value> {
    let Json(request) = request.map_err(|_| invalid("invalid shutdown cancellation JSON"))?;
    binding(&context.control, &peer, &request.process_nonce)?;
    let mut operation = context.control.operation.lock().await;
    let prepared = validate_preparation(&mut operation, &request, &peer)?;
    if prepared.committed {
        return Err(unsafe_shutdown("committed shutdown cannot be cancelled"));
    }
    let prepared = operation.take().expect("validated preparation");
    if let Some(lease) = prepared.lease {
        lease.release().await;
    } else {
        state
            .serving
            .cancel_planned_outage_preparation(prepared.token, 0)
            .await;
    }
    Ok(Json(
        serde_json::json!({"schema_version":1,"cancelled":true}),
    ))
}
fn validate_preparation<'a>(
    operation: &'a mut Option<Preparation>,
    request: &ShutdownCommitRequestV1,
    peer: &Peer,
) -> Result<&'a mut Preparation, LocalError> {
    let prepared = operation
        .as_mut()
        .ok_or_else(|| unsafe_shutdown("there is no prepared operation"))?;
    if request.schema_version != 1
        || prepared.operation_id != request.operation_id
        || prepared.generation != request.generation
        || prepared.capability != request.owner_capability
        || prepared.peer.uid != peer.uid
        || prepared.peer.pid != peer.pid
        || prepared.peer.generation != peer.generation
        || prepared.peer.connection_id != peer.connection_id
    {
        return Err(LocalError(
            StatusCode::FORBIDDEN,
            "authorization_denied",
            "operation ownership or generation does not match",
        ));
    }
    if !prepared.committed && prepared.expires <= unix_ms() {
        return Err(LocalError(
            StatusCode::GONE,
            "unsafe_shutdown",
            "preparation expired; prepare again",
        ));
    }
    Ok(prepared)
}
async fn current_topology(state: &AppState) -> Result<MembershipStatus, LocalError> {
    if !state.serving.is_ready() {
        return Err(unsafe_shutdown(
            "local serving authority is unavailable; use administrator maintenance",
        ));
    }
    let status = tokio::time::timeout(Duration::from_secs(5), state.membership.status())
        .await
        .map_err(|_| unsafe_shutdown("topology observation timed out"))?
        .map_err(|_| unsafe_shutdown("topology is unknown; use administrator maintenance"))?;
    if status.replication.backend != ReplicationBackend::Replicated
        || status.replication.health != ReplicationHealth::InSync
        || status.recovery.required
        || !status.recovery.quorum_available
        || status.local_node_id != state.node_id
    {
        return Err(unsafe_shutdown(
            "recovery, activation or stale topology prevents safe local shutdown",
        ));
    }
    Ok(status)
}
fn topology_allowed(
    status: &MembershipStatus,
    node: &str,
    rolling_safe: bool,
    candidate: Option<&str>,
) -> Result<(), LocalError> {
    if status.local_node_id != node
        || status.replication.backend != ReplicationBackend::Replicated
        || status.replication.health != ReplicationHealth::InSync
        || status.recovery.required
        || !status.recovery.quorum_available
        || !status.recovery.leader_elected
    {
        return Err(unsafe_shutdown(
            "recovery, activation or stale topology prevents safe local shutdown",
        ));
    }
    let local = status
        .nodes
        .iter()
        .find(|n| n.node_id == node)
        .ok_or_else(|| unsafe_shutdown("local member is absent from committed topology"))?;
    if local.removal_pending
        || local.maintenance
        || status.protocol.active_min < status.protocol.binary_min
        || status.protocol.active_max > status.protocol.binary_max
    {
        return Err(unsafe_shutdown(
            "membership role or protocol compatibility prevents safe shutdown",
        ));
    }
    let sole_voter = status.capacity.voting_nodes == 1
        && status.capacity.non_voting_replicas == 0
        && status.nodes.len() == 1
        && local.is_voter
        && local.role == plurx_core::cluster::membership::NodeRole::Voter;
    if sole_voter || (rolling_safe && candidate == Some(node)) {
        Ok(())
    } else {
        Err(unsafe_shutdown(
            "the current rolling restart verdict does not select this member",
        ))
    }
}
fn topology_generation(status: &MembershipStatus) -> String {
    let mut nodes: Vec<_> = status
        .nodes
        .iter()
        .map(|n| {
            (
                &n.node_id,
                serde_json::to_string(&n.role).expect("role serialization"),
                n.is_voter,
                n.removal_pending,
                n.maintenance,
                n.learner_protocol_ready,
            )
        })
        .collect();
    nodes.sort();
    hex::encode(Sha256::digest(
        serde_json::to_vec(&(
            nodes,
            status.protocol.active_min,
            status.protocol.active_max,
        ))
        .expect("topology serialization"),
    ))
}
fn random_capability() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}
fn unix_ms() -> u64 {
    crate::media_sessions::unix_ms().try_into().unwrap_or(0)
}
fn classify_exit(http_drained: bool, store_closed: bool) -> ExitClass {
    match (http_drained, store_closed) {
        (true, true) => ExitClass::Clean,
        (false, true) => ExitClass::DrainFailed,
        (true, false) => ExitClass::StoreFailed,
        (false, false) => ExitClass::Unclean,
    }
}
fn validate_directory(path: &Path, owner: u32) -> anyhow::Result<()> {
    ensure!(
        path.is_absolute(),
        "control state must use an absolute path"
    );
    let leaf = std::fs::symlink_metadata(path)?;
    ensure!(
        leaf.is_dir()
            && !leaf.file_type().is_symlink()
            && leaf.uid() == owner
            && leaf.mode() & 0o022 == 0,
        "control directory has unsafe ownership or permissions"
    );
    for parent in path.ancestors().skip(1) {
        let metadata = std::fs::symlink_metadata(parent)?;
        ensure!(
            metadata.is_dir()
                && !metadata.file_type().is_symlink()
                && (metadata.uid() == 0 || metadata.uid() == owner)
                && metadata.mode() & 0o022 == 0,
            "control ancestor has unsafe ownership or permissions"
        );
    }
    Ok(())
}
fn write_receipt(directory: &Path, receipt: &ShutdownReceiptV1) -> anyhow::Result<()> {
    use std::io::Write;
    validate_directory(directory, receipt.process.uid)?;
    uuid::Uuid::parse_str(&receipt.operation_id)?;
    let temporary = directory.join(format!(
        ".{}.{}.tmp",
        receipt.operation_id,
        uuid::Uuid::new_v4()
    ));
    let final_path = directory.join(format!("{}.json", receipt.operation_id));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)?;
    let bytes = serde_json::to_vec(receipt)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    std::fs::rename(&temporary, final_path)?;
    std::fs::File::open(directory)?.sync_all()?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn process_generation(pid: u32) -> anyhow::Result<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    // comm may contain spaces and parentheses; the last ')' terminates it.
    stat.rsplit_once(')')
        .and_then(|(_, fields)| fields.split_whitespace().nth(19))
        .map(str::to_owned)
        .context("invalid process start generation")
}
#[cfg(target_os = "macos")]
fn process_generation(pid: u32) -> anyhow::Result<String> {
    // SAFETY: zeroed C record of the exact size supplied to proc_pidinfo.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of_val(&info);
    let result = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            size as libc::c_int,
        )
    };
    ensure!(
        result == size as libc::c_int && info.pbi_pid == pid,
        "cannot prove process generation"
    );
    Ok(format!(
        "{}:{}",
        info.pbi_start_tvsec, info.pbi_start_tvusec
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use plurx_core::cluster::migration::status::ReplicationMonitor;
    use tower::ServiceExt;

    fn peer() -> Peer {
        Peer {
            uid: unsafe { libc::geteuid() },
            pid: std::process::id(),
            generation: process_generation(std::process::id()).expect("desktop control fixture"),
            connection_id: "test-held-connection".into(),
        }
    }
    fn control(receipts: PathBuf) -> Arc<DesktopControl> {
        let peer = peer();
        Arc::new(DesktopControl {
            installation_id: uuid::Uuid::new_v4().to_string(),
            socket: receipts.join("socket"),
            receipts,
            process: ProcessIdentityV1 {
                pid: peer.pid,
                uid: peer.uid,
                start_generation: peer.generation,
                nonce: random_capability(),
            },
            supervisor: SupervisorIdentityV1 {
                kind: SupervisorKind::Launchd,
                name: "com.plurx.plurxd".into(),
                invocation_id: "test".into(),
            },
            operation: tokio::sync::Mutex::new(None),
            http_drained: AtomicBool::new(false),
            auxiliary_drained: AtomicBool::new(true),
        })
    }
    fn context(control: Arc<DesktopControl>) -> Arc<ContextState> {
        Arc::new(ContextState {
            identity: DaemonIdentityV1 {
                schema_version: 1,
                installation_id: control.installation_id.clone(),
                process: control.process.clone(),
                node_id: "node-1".into(),
                instance_id: "instance".into(),
                version: "0.7.0".into(),
                build: "test".into(),
                browser_origin: "http://127.0.0.1:8080".into(),
                supervisor: control.supervisor.clone(),
                ready: true,
            },
            control,
        })
    }
    async fn prepared(
        fence: &crate::serving_fence::ServingFence,
        expires: u64,
    ) -> (Preparation, ShutdownCommitRequestV1) {
        let token = fence
            .begin_desktop_preparation_until(expires)
            .await
            .expect("desktop control fixture");
        let operation_id = uuid::Uuid::new_v4().to_string();
        let capability = random_capability();
        let generation = "topology-1".to_owned();
        let request = ShutdownCommitRequestV1 {
            schema_version: 1,
            operation_id: operation_id.clone(),
            process_nonce: "nonce".into(),
            generation: generation.clone(),
            owner_capability: capability.clone(),
        };
        (
            Preparation {
                operation_id,
                generation,
                capability,
                expires,
                token,
                lease: None,
                committed: false,
                peer: peer(),
            },
            request,
        )
    }
    fn membership(voters: usize) -> MembershipStatus {
        let nodes: Vec<_> = (1..=voters).map(|i| serde_json::json!({"node_id":format!("node-{i}"),"hostname":format!("node-{i}"),"advertised_host":"localhost","raft_id":i,"role":"voter","is_voter":true,"is_leader":i==1,"reachable":true,"last_seen_at":1,"removal_pending":false,"learner_protocol_ready":true})).collect();
        serde_json::from_value(serde_json::json!({"local_node_id":"node-1","availability":"single_node","nodes":nodes,
            "replication":{"backend":"replicated","health":"in_sync","clustered":voters>1,"last_applied_term":1,"last_applied_index":1,"checked_at":1,"explanation":"test"},
            "capacity":{"voting_nodes":voters,"voting_quorum":voters/2+1,"voting_failure_tolerance":0,"non_voting_replicas":0,"ready_read_workers":0},
            "protocol":{"active_min":1,"active_max":1,"binary_min":1,"binary_max":1,"learner_protocol_active":false,"learner_protocol_pending":[]},
            "recovery":{"required":false,"quorum_available":true,"reachable_voters":voters,"required_voters":voters/2+1,"leader_elected":true,"permanent_majority_loss_supported":false}
        })).expect("desktop control fixture")
    }

    #[test]
    fn native_sole_voter_downtime_and_rolling_candidates_have_distinct_rules() {
        let sole = membership(1);
        assert!(topology_allowed(&sole, "node-1", false, None).is_ok());
        let multi = membership(3);
        assert!(topology_allowed(&multi, "node-1", false, None).is_err());
        assert!(topology_allowed(&multi, "node-1", true, Some("node-2")).is_err());
        assert!(topology_allowed(&multi, "node-1", true, Some("node-1")).is_ok());
        let mut learner = sole.clone();
        learner.nodes[0].is_voter = false;
        learner.capacity.non_voting_replicas = 1;
        assert!(topology_allowed(&learner, "node-1", false, None).is_err());
        let mut removed = sole.clone();
        removed.nodes[0].removal_pending = true;
        assert!(topology_allowed(&removed, "node-1", true, Some("node-1")).is_err());
        let mut recovery = sole.clone();
        recovery.recovery.required = true;
        assert!(topology_allowed(&recovery, "node-1", true, Some("node-1")).is_err());
        let mut stale = sole.clone();
        stale.replication.health = ReplicationHealth::Degraded;
        assert!(topology_allowed(&stale, "node-1", true, Some("node-1")).is_err());
        let mut sqlite = sole.clone();
        sqlite.replication.backend = ReplicationBackend::Sqlite;
        assert!(topology_allowed(&sqlite, "node-1", true, Some("node-1")).is_err());
        let mut incompatible = sole;
        incompatible.protocol.active_max = 99;
        assert!(topology_allowed(&incompatible, "node-1", true, Some("node-1")).is_err());
    }

    #[test]
    fn generation_binds_membership_and_protocol_without_heartbeat_churn() {
        let mut status = membership(1);
        let generation = topology_generation(&status);
        status.nodes[0].last_seen_at += 100;
        status.nodes[0].active_media_sessions = 2;
        assert_eq!(generation, topology_generation(&status));
        status.nodes[0].is_voter = false;
        assert_ne!(generation, topology_generation(&status));
        status.nodes[0].is_voter = true;
        status.protocol.active_max += 1;
        assert_ne!(generation, topology_generation(&status));
    }

    #[tokio::test]
    async fn kernel_peer_and_fresh_nonce_reject_process_generation_reuse() {
        let (one, _two) = UnixStream::pair().expect("desktop control fixture");
        let actual = Peer::from_socket(&one).expect("desktop control fixture");
        assert_eq!(actual.pid, std::process::id());
        assert_eq!(actual.uid, peer().uid);
        assert!(actual.current());
        let control = control(PathBuf::new());
        assert!(binding(&control, &actual, &control.process.nonce).is_ok());
        assert!(binding(&control, &actual, "old process nonce").is_err());
        let reused = Peer {
            generation: "old generation".into(),
            ..actual
        };
        assert!(binding(&control, &reused, &control.process.nonce).is_err());
        assert_ne!(
            control.process.nonce,
            super::tests::control(PathBuf::new()).process.nonce
        );
    }

    #[tokio::test]
    async fn anonymous_nonowner_cannot_mutate_local_server() {
        let state = crate::http::source_actor_test_state();
        let control = control(PathBuf::new());
        let unauthorized_peer = Peer {
            uid: control.process.uid.saturating_add(1),
            ..peer()
        };
        let app = Router::new()
            .route(
                "/local/v1/shutdown/prepare",
                post(|| async { StatusCode::NO_CONTENT }),
            )
            .route_layer(middleware::from_fn_with_state(state.clone(), authorize))
            .layer(Extension(context(control)))
            .layer(Extension(unauthorized_peer))
            .with_state(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/local/v1/shutdown/prepare")
                    .body(axum::body::Body::empty())
                    .expect("desktop control fixture"),
            )
            .await
            .expect("desktop control fixture");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn unix_identity_and_auth_use_one_kernel_bound_connection_and_no_tcp_route() {
        let directory =
            tempfile::tempdir_in(std::env::current_dir().expect("desktop control fixture"))
                .expect("desktop control fixture");
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
            .expect("desktop control fixture");
        let control = control(directory.path().to_owned());
        let state = crate::http::source_actor_test_state();
        let server = control
            .serve(state.clone(), "http://127.0.0.1:8080".into())
            .await
            .expect("desktop control fixture");
        let socket = UnixStream::connect(&control.socket)
            .await
            .expect("desktop control fixture");
        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(socket))
            .await
            .expect("desktop control fixture");
        let connection = tokio::spawn(connection);
        let response = sender
            .send_request(
                Request::builder()
                    .uri("/local/v1/identity")
                    .body(axum::body::Body::empty())
                    .expect("desktop control fixture"),
            )
            .await
            .expect("desktop control fixture");
        assert_eq!(response.status(), StatusCode::OK);
        use http_body_util::BodyExt;
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("desktop control fixture")
            .to_bytes();
        let identity: DaemonIdentityV1 =
            serde_json::from_slice(&bytes).expect("desktop control fixture");
        assert_eq!(identity.process, control.process);
        assert_eq!(identity.build, crate::version::SOURCE_COMMIT);
        assert_eq!(identity.version, crate::version::SEMVER);
        let response = sender
            .send_request(
                Request::builder()
                    .method("POST")
                    .uri("/local/v1/shutdown/prepare")
                    .header("Content-Type", "application/json")
                    .body(axum::body::Body::from("{}"))
                    .expect("desktop control fixture"),
            )
            .await
            .expect("desktop control fixture");
        if unsafe { libc::geteuid() } != 0 {
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }
        let tcp_response = crate::http::router(state)
            .oneshot(
                Request::builder()
                    .uri("/local/v1/identity")
                    .body(axum::body::Body::empty())
                    .expect("desktop control fixture"),
            )
            .await
            .expect("desktop control fixture");
        assert_eq!(tcp_response.status(), StatusCode::NOT_FOUND);
        drop(response);
        drop(sender);
        connection
            .await
            .expect("desktop control fixture")
            .expect("desktop control fixture");
        server.finish().await.expect("desktop control fixture");
    }

    #[test]
    fn protected_runtime_directories_reject_symlinks_and_shared_write_access() {
        let root = tempfile::tempdir_in(std::env::current_dir().expect("desktop control fixture"))
            .expect("desktop control fixture");
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))
            .expect("desktop control fixture");
        let directory = root.path().join("owned");
        std::fs::create_dir(&directory).expect("desktop control fixture");
        assert!(validate_directory(&directory, peer().uid).is_ok());
        let alias = root.path().join("alias");
        std::os::unix::fs::symlink(&directory, &alias).expect("desktop control fixture");
        assert!(validate_directory(&alias, peer().uid).is_err());
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o770))
            .expect("desktop control fixture");
        assert!(validate_directory(&directory, peer().uid).is_err());
    }

    #[tokio::test]
    async fn final_preparation_response_cannot_authorize_after_expiry_or_owner_change() {
        let fence =
            crate::serving_fence::ServingFence::new(ReplicationMonitor::sqlite().metrics_handle());
        let (preparation, mut request) = prepared(&fence, unix_ms() + 30_000).await;
        let mut operation = Some(preparation);
        assert!(validate_preparation(&mut operation, &request, &peer()).is_ok());
        request.owner_capability = "stolen public status".into();
        assert!(validate_preparation(&mut operation, &request, &peer()).is_err());
        request.owner_capability = operation
            .as_ref()
            .expect("desktop control fixture")
            .capability
            .clone();
        let reused = Peer {
            generation: "reused PID".into(),
            ..peer()
        };
        assert!(validate_preparation(&mut operation, &request, &reused).is_err());
        let reconnected = Peer {
            connection_id: "new verified connection".into(),
            ..peer()
        };
        assert!(validate_preparation(&mut operation, &request, &reconnected).is_err());
        operation
            .as_mut()
            .expect("desktop control fixture")
            .peer
            .generation
            .clear();
        let restricted = Peer {
            generation: String::new(),
            ..peer()
        };
        assert!(validate_preparation(&mut operation, &request, &restricted).is_ok());
        let restricted_reconnect = Peer {
            connection_id: "another restricted connection".into(),
            ..restricted.clone()
        };
        assert!(validate_preparation(&mut operation, &request, &restricted_reconnect).is_err());
        operation.as_mut().expect("desktop control fixture").expires = unix_ms() - 1;
        assert_eq!(
            validate_preparation(&mut operation, &request, &restricted)
                .err()
                .expect("desktop control fixture")
                .0,
            StatusCode::GONE
        );
    }

    #[tokio::test]
    async fn unknown_or_sqlite_recovery_never_enters_native_downtime() {
        let state = crate::http::source_actor_test_state();
        assert!(current_topology(&state).await.is_err());
        assert!(!state.shutdown.is_cancelled());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn native_uninitialized_and_configured_sole_voter_commit_real_replicated_store() {
        for configured in [false, true] {
            let root =
                tempfile::tempdir_in(std::env::current_dir().expect("desktop control fixture"))
                    .expect("desktop control fixture");
            std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))
                .expect("desktop control fixture");
            let listeners: Vec<_> = (0..3)
                .map(|_| {
                    std::net::TcpListener::bind("127.0.0.1:0").expect("desktop control fixture")
                })
                .collect();
            let addresses: Vec<_> = listeners
                .iter()
                .map(|listener| listener.local_addr().expect("desktop control fixture"))
                .collect();
            let mut config = plurx_core::config::Config::default();
            config.storage.data_dir = root.path().join("data");
            std::fs::create_dir_all(&config.storage.data_dir).expect("desktop control fixture");
            config.server.bind = addresses[0];
            config.cluster.raft_bind = addresses[1];
            config.cluster.api_bind = addresses[2];
            config.cluster.advertise_host = "localhost".into();
            drop(listeners);
            drop(
                plurx_core::store::SqliteStore::open(&config.storage.data_dir.join("plurx.db"))
                    .expect("desktop control fixture"),
            );
            let observation = Arc::new(crate::StartupObservationHttp::new(config.server.bind));
            let selected = plurx_core::cluster::migration::select_daemon_store_observing(
                &config,
                Some(observation.as_ref()),
            )
            .await
            .expect("desktop control fixture");
            assert!(matches!(
                selected.backend,
                plurx_core::cluster::migration::SelectedBackend::Replicated
            ));
            let store = Arc::clone(&selected.store);
            if configured {
                store
                    .create_user("native-admin", "synthetic test hash", true)
                    .await
                    .expect("desktop control fixture");
            }
            let mut state = crate::build_state(
                &config,
                selected.identity.node_id.clone(),
                selected.identity.cluster_id.clone(),
                Arc::clone(&selected.credential_key),
                selected.local_client(),
                selected.replication_monitor(),
                selected.membership_manager(),
                selected.catalogue_reader(),
                store,
                crate::create_dirs_for_config(&config).expect("desktop control fixture"),
                Default::default(),
                Default::default(),
                crate::logbuf::LogBuffers {
                    general: Arc::new(crate::logbuf::LogBuffer::new(32)),
                    cluster: Arc::new(crate::logbuf::LogBuffer::new(32)),
                },
            );
            state.clock_observer = observation.observer().await.expect("startup clock owner");
            state
                .membership
                .heartbeat()
                .await
                .expect("desktop control fixture");
            let stop = CancellationToken::new();
            let metrics = tokio::spawn(
                state
                    .replication
                    .clone()
                    .passive_metrics_loop(stop.clone().cancelled_owned()),
            );
            let serving = tokio::spawn(state.serving.clone().monitor_loop(stop.clone()));
            tokio::time::timeout(Duration::from_secs(8), async {
                while !state.serving.is_ready() {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            })
            .await
            .expect("fresh sole-voter serving proof");
            let control = control(root.path().to_owned());
            let context = context(Arc::clone(&control));
            #[cfg(target_os = "linux")]
            let native_peer = Peer { uid: 0, ..peer() };
            #[cfg(target_os = "macos")]
            let native_peer = peer();
            let request = ShutdownPrepareRequestV1 {
                schema_version: 1,
                operation_id: uuid::Uuid::new_v4().to_string(),
                operation: Operation::Restart,
                process_nonce: control.process.nonce.clone(),
                confirm_interruption: true,
                target_version: None,
            };
            let prepared = match prepare_owned(
                state.clone(),
                Arc::clone(&context),
                native_peer.clone(),
                request,
            )
            .await
            {
                Ok(Json(prepared)) => prepared,
                Err(error) => panic!("real sole-voter preparation: {}", error.2),
            };
            let commit = ShutdownCommitRequestV1 {
                schema_version: 1,
                operation_id: prepared.operation_id,
                process_nonce: prepared.process_nonce,
                generation: prepared.generation,
                owner_capability: prepared.owner_capability,
            };
            let operation_id = commit.operation_id.clone();
            let committed = match commit_owned(state.clone(), context, native_peer, commit).await {
                Ok(Json(committed)) => committed,
                Err(error) => panic!("real sole-voter commit: {}", error.2),
            };
            assert!(committed.committed);
            assert!(state.shutdown.is_cancelled());
            assert!(state.serving.try_restart_admission().await.is_none());
            // Closing the controller leaves the committed daemon owner alive.
            stop.cancel();
            metrics.await.expect("desktop control fixture");
            serving.await.expect("desktop control fixture");
            observation.stop_and_drain().await;
            selected.shutdown().await.expect("desktop control fixture");
            control.record_http_drain(true);
            control.finish(true).await.expect("desktop control fixture");
            let receipt: ShutdownReceiptV1 = serde_json::from_slice(
                &std::fs::read(root.path().join(format!("{operation_id}.json")))
                    .expect("desktop control fixture"),
            )
            .expect("desktop control fixture");
            assert_eq!(receipt.exit_class, ExitClass::Clean);
        }
    }

    #[test]
    fn failed_http_drain_and_store_close_are_never_clean_exit_authority() {
        assert_eq!(classify_exit(true, true), ExitClass::Clean);
        assert_eq!(classify_exit(false, true), ExitClass::DrainFailed);
        assert_eq!(classify_exit(true, false), ExitClass::StoreFailed);
        assert_eq!(classify_exit(false, false), ExitClass::Unclean);
    }

    #[tokio::test]
    async fn committed_shutdown_writes_exact_durable_receipt_only_after_store_close() {
        // HOME is private on native installs; temporary test roots are often
        // below a system-writable /tmp and deliberately fail path policy.
        // Use the test-owned working directory, whose ancestors are protected.
        let dir = tempfile::tempdir_in(std::env::current_dir().expect("desktop control fixture"))
            .expect("desktop control fixture");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
            .expect("desktop control fixture");
        let control = control(dir.path().to_owned());
        let fence =
            crate::serving_fence::ServingFence::new(ReplicationMonitor::sqlite().metrics_handle());
        let (mut preparation, _) = prepared(&fence, unix_ms() + 30_000).await;
        preparation.committed = true;
        let operation_id = preparation.operation_id.clone();
        *control.operation.lock().await = Some(preparation);
        control.record_http_drain(true);
        control.finish(true).await.expect("desktop control fixture");
        let receipt: ShutdownReceiptV1 = serde_json::from_slice(
            &std::fs::read(dir.path().join(format!("{operation_id}.json")))
                .expect("desktop control fixture"),
        )
        .expect("desktop control fixture");
        assert_eq!(receipt.process, control.process);
        assert_eq!(receipt.exit_class, ExitClass::Clean);
        assert!(control.finish(false).await.is_err());
        let receipt: ShutdownReceiptV1 = serde_json::from_slice(
            &std::fs::read(dir.path().join(format!("{operation_id}.json")))
                .expect("desktop control fixture"),
        )
        .expect("desktop control fixture");
        assert_eq!(receipt.exit_class, ExitClass::StoreFailed);
    }
}
