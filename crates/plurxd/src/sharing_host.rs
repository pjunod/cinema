//! Node-local host reconciliation. Host facts are observations, never authority
//! to disable Sharing or admit a peer. Docker receives no privileged socket.
use crate::state::{clock_ms, AppState};
use plurx_core::{
    config::SharingListenerProfile,
    sharing::{is_tailnet_address, validate_endpoints, Endpoint, MutationOutcome},
};
use serde::{Deserialize, Serialize};
use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

const LIMIT: usize = 256 * 1024;
const TARGET: &str = "127.0.0.1:32444";
const PORT: u16 = 32443;
const POLL: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HostMode {
    ManagedNative,
    UnmanagedNative,
    ExternalContainer,
}
impl HostMode {
    fn select(managed: bool, container: bool) -> Self {
        if container {
            Self::ExternalContainer
        } else if managed {
            Self::ManagedNative
        } else {
            Self::UnmanagedNative
        }
    }
}

#[cfg(any(target_os = "macos", test))]
fn legacy_launchd_identity(output: &[u8], service: &str) -> Option<(u32, PathBuf, PathBuf)> {
    let text = std::str::from_utf8(output).ok()?;
    if !text.trim_end().ends_with("\n}") {
        return None;
    }
    let mut lines = text.lines();
    if lines.next()? != format!("{service} = {{") {
        return None;
    }
    let (mut pid, mut program, mut path) = (None, None, None);
    for line in lines {
        // launchctl indents first-level attributes with one tab. Nested
        // arguments/environment values cannot supply process identity.
        if let Some(value) = line.strip_prefix("\tpid = ") {
            if pid.is_some() {
                return None;
            }
            pid = Some(value.parse::<u32>().ok()?);
        } else if let Some(value) = line.strip_prefix("\tprogram = ") {
            if program.is_some() || value.is_empty() {
                return None;
            }
            program = Some(PathBuf::from(value));
        } else if let Some(value) = line.strip_prefix("\tpath = ") {
            if path.is_some() || value.is_empty() {
                return None;
            }
            path = Some(PathBuf::from(value));
        }
    }
    Some((pid.filter(|pid| *pid > 1)?, program?, path?))
}

#[cfg(all(unix, any(target_os = "macos", test)))]
fn legacy_service_matches(
    output: &[u8],
    service: &str,
    pid: u32,
    executable: &Path,
    registration: &Path,
    uid: u32,
) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Some((observed_pid, program, path)) = legacy_launchd_identity(output, service) else {
        return false;
    };
    if observed_pid != pid || path != registration || uid == 0 {
        return false;
    }
    let Ok(definition) = std::fs::symlink_metadata(registration) else {
        return false;
    };
    if !definition.is_file() || definition.uid() != uid || definition.mode() & 0o022 != 0 {
        return false;
    }
    let (Ok(program), Ok(executable)) = (program.canonicalize(), executable.canonicalize()) else {
        return false;
    };
    let Ok(binary) = std::fs::metadata(&executable) else {
        return false;
    };
    program == executable
        && binary.is_file()
        && [0, uid].contains(&binary.uid())
        && binary.mode() & 0o022 == 0
}

#[cfg(target_os = "macos")]
async fn legacy_native_service() -> bool {
    // A legacy --server-only LaunchAgent has no desktop-control environment.
    // Prove that launchd owns this exact process before allowing host changes.
    // SAFETY: geteuid has no pointer arguments or side effects.
    let uid = unsafe { libc::geteuid() };
    let Ok(home) = plurx_desktop_control::installation::home_for(uid) else {
        return false;
    };
    let Ok(executable) = std::env::current_exe() else {
        return false;
    };
    let service = format!("gui/{uid}/com.plurx.plurxd");
    let Ok(output) = command_with_timeout(
        Path::new("/bin/launchctl"),
        &["print", &service],
        Duration::from_secs(2),
    )
    .await
    else {
        return false;
    };
    legacy_service_matches(
        &output,
        &service,
        std::process::id(),
        &executable,
        &home.join("Library/LaunchAgents/com.plurx.plurxd.plist"),
        uid,
    )
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SetupState {
    #[default]
    Pending,
    Disabled,
    Ready,
    TailscaleMissing,
    SignInRequired,
    PermissionRequired,
    PortConflict,
    HostHelperRequired,
    NativeSetupRequired,
    DeploymentRequired,
    Unavailable,
}
#[derive(Clone, Default, Serialize)]
pub(crate) struct SetupStatus {
    pub state: SetupState,
    pub message: String,
    pub checked_at_ms: Option<i64>,
}
impl SetupState {
    fn status(self) -> SetupStatus {
        SetupStatus { state: self, checked_at_ms: Some(clock_ms()), message: match self {
            Self::Pending => "Setting up the private connection…",
            Self::Disabled => "Sharing is disabled.",
            Self::Ready => "Private host setup is configured. Pair another Cinema to verify the connection.",
            Self::TailscaleMissing => "Install Tailscale on this Cinema's host, then sign in. Cinema will retry automatically.",
            Self::SignInRequired => "Open Tailscale on this Cinema's host and sign in. Cinema will retry automatically.",
            Self::PermissionRequired => "Allow Cinema's host setup component to configure Tailscale, then retry.",
            Self::PortConflict => "Another Tailscale rule uses Cinema's private port. Cinema has preserved that rule; resolve the conflict on this host.",
            Self::HostHelperRequired => "Install Cinema's sharing host component on the Docker host. The container cannot configure the host's Tailscale service.",
            Self::NativeSetupRequired => "Native sharing needs Cinema's listener at 127.0.0.1:32444 and private TCP forwarding from port 32443. Start the installed Cinema service for automatic setup, or configure this forwarding on the host.",
            Self::DeploymentRequired => "The Docker host setup must publish Cinema's sharing listener on host loopback and select its private bridge.",
            Self::Unavailable => "Private host setup could not be verified. Check Tailscale on this host; Cinema will retry automatically.",
        }.into() }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HostAddress {
    ipv4: Ipv4Addr,
    ipv6: Option<Ipv6Addr>,
    ts_fqdn: String,
}
impl HostAddress {
    fn endpoint(&self, pin: String) -> Result<Endpoint, SetupState> {
        let endpoint = Endpoint {
            ipv4: self.ipv4,
            ipv6: self.ipv6,
            ts_fqdn: self.ts_fqdn.clone(),
            port: PORT,
            spki_sha256: pin,
        };
        validate_endpoints(std::slice::from_ref(&endpoint)).map_err(|_| SetupState::Unavailable)?;
        Ok(endpoint)
    }
}
#[derive(Serialize)]
struct HostRequest<'a> {
    schema_version: u8,
    generation: &'a str,
    node_id: &'a str,
    enabled: bool,
    checked_at_ms: i64,
    listener_profile: &'static str,
    listener_address: String,
    container_egress: Option<IpAddr>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HostReport {
    schema_version: u8,
    generation: String,
    node_id: String,
    enabled: bool,
    checked_at_ms: i64,
    state: SetupState,
    address: Option<HostAddress>,
    egress_guard: bool,
}

pub(crate) fn private_directory(path: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    let m = std::fs::symlink_metadata(path)?;
    if !m.is_dir() {
        return Err(std::io::ErrorKind::PermissionDenied.into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if m.mode() & 0o077 != 0 || m.uid() != unsafe { libc::geteuid() } {
            return Err(std::io::ErrorKind::PermissionDenied.into());
        }
    }
    Ok(())
}
pub(crate) fn write_json(path: &Path, value: &impl Serialize) -> std::io::Result<()> {
    use std::io::Write;
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = options.open(&temporary)?;
        file.write_all(&serde_json::to_vec(value)?)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}
pub(crate) fn read_json(path: &Path) -> Result<serde_json::Value, SetupState> {
    read_json_owned(path, false)
}
#[cfg(unix)]
fn owner_allowed(owner: u32, daemon: u32, host_report: bool) -> bool {
    if host_report {
        owner == 0
    } else {
        owner == 0 || owner == daemon
    }
}
fn read_json_owned(path: &Path, host_report: bool) -> Result<serde_json::Value, SetupState> {
    #[cfg(not(unix))]
    if host_report {
        return Err(SetupState::HostHelperRequired);
    }
    use std::io::Read;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|_| SetupState::Unavailable)?;
    let metadata = file.metadata().map_err(|_| SetupState::Unavailable)?;
    if !metadata.is_file() || metadata.len() > LIMIT as u64 {
        return Err(SetupState::Unavailable);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.mode() & 0o022 != 0
            || !owner_allowed(metadata.uid(), unsafe { libc::geteuid() }, host_report)
        {
            return Err(SetupState::Unavailable);
        }
    }
    let mut bytes = Vec::new();
    file.take((LIMIT + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| SetupState::Unavailable)?;
    if bytes.len() > LIMIT {
        return Err(SetupState::Unavailable);
    }
    serde_json::from_slice(&bytes).map_err(|_| SetupState::Unavailable)
}

fn tailscale_program() -> Result<PathBuf, SetupState> {
    #[cfg(target_os = "macos")]
    let paths = [
        "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
        "/opt/homebrew/bin/tailscale",
        "/usr/local/bin/tailscale",
    ];
    #[cfg(target_os = "windows")]
    let paths = [r"C:\Program Files\Tailscale\tailscale.exe"];
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let paths = ["/usr/bin/tailscale", "/usr/local/bin/tailscale"];
    for path in paths {
        let Ok(path) = std::fs::canonicalize(path) else {
            continue;
        };
        let Ok(m) = std::fs::metadata(&path) else {
            continue;
        };
        if !m.is_file() {
            continue;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if m.mode() & 0o022 != 0 || ![0, unsafe { libc::geteuid() }].contains(&m.uid()) {
                continue;
            }
        }
        return Ok(path);
    }
    Err(SetupState::TailscaleMissing)
}
async fn command(program: &Path, args: &[&str]) -> Result<Vec<u8>, SetupState> {
    command_with_timeout(program, args, Duration::from_secs(8)).await
}

async fn command_with_timeout(
    program: &Path,
    args: &[&str],
    deadline: Duration,
) -> Result<Vec<u8>, SetupState> {
    let mut process = tokio::process::Command::new(program);
    process.args(args);
    // Host tooling keeps its existing user/IPC environment. The shared owner
    // isolates its process group and reaps it before timeout/cancellation returns.
    let output = plurx_core::process::bounded::output_command_inherited_env(
        &mut process,
        deadline,
        LIMIT + 1,
        crate::process_control::ChildWork::background("private sharing host setup"),
    )
    .await
    .map_err(|error| match error.kind() {
        std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::NotFound => {
            SetupState::PermissionRequired
        }
        _ => SetupState::Unavailable,
    })?;
    if output.stdout.len() > LIMIT {
        return Err(SetupState::Unavailable);
    }
    if !output.status.success() {
        return Err(SetupState::PermissionRequired);
    }
    Ok(output.stdout)
}
async fn command_json(program: &Path, args: &[&str]) -> Result<serde_json::Value, SetupState> {
    serde_json::from_slice(&command(program, args).await?).map_err(|_| SetupState::Unavailable)
}
fn host_address(status: &serde_json::Value) -> Result<HostAddress, SetupState> {
    if status["BackendState"] != "Running" {
        return Err(SetupState::SignInRequired);
    }
    let self_node = &status["Self"];
    let ips: Vec<IpAddr> = serde_json::from_value(self_node["TailscaleIPs"].clone())
        .map_err(|_| SetupState::Unavailable)?;
    let ipv4 = ips
        .iter()
        .find_map(|ip| match ip {
            IpAddr::V4(ip) if is_tailnet_address((*ip).into()) => Some(*ip),
            _ => None,
        })
        .ok_or(SetupState::Unavailable)?;
    let ipv6 = ips.iter().find_map(|ip| match ip {
        IpAddr::V6(ip) if is_tailnet_address((*ip).into()) => Some(*ip),
        _ => None,
    });
    let ts_fqdn = self_node["DNSName"]
        .as_str()
        .ok_or(SetupState::Unavailable)?
        .trim_end_matches('.')
        .to_owned();
    let address = HostAddress {
        ipv4,
        ipv6,
        ts_fqdn,
    };
    address.endpoint("0".repeat(64))?;
    Ok(address)
}
#[derive(Debug, PartialEq, Eq)]
enum ServeRule {
    Missing,
    Ours,
    Conflict,
}
fn serve_rule(config: &serde_json::Value) -> ServeRule {
    // Never adopt a public forward or a foreground rule we do not own.
    if config
        .get("Foreground")
        .and_then(|v| v.as_object())
        .is_some_and(|v| {
            v.values().any(|foreground| {
                foreground
                    .get("TCP")
                    .and_then(|tcp| tcp.get("32443"))
                    .is_some()
            })
        })
        || config
            .get("AllowFunnel")
            .and_then(|v| v.as_object())
            .is_some_and(|v| v.iter().any(|(k, v)| k.ends_with(":32443") && v == true))
    {
        return ServeRule::Conflict;
    }
    let Some(rule) = config.get("TCP").and_then(|v| v.get(PORT.to_string())) else {
        return ServeRule::Missing;
    };
    if rule == &serde_json::json!({"TCPForward": TARGET}) {
        ServeRule::Ours
    } else {
        ServeRule::Conflict
    }
}
async fn native_setup(
    directory: &Path,
    enabled: bool,
    state: &AppState,
    discovered: &mut Option<IpAddr>,
    managed: bool,
) -> Result<Option<HostAddress>, SetupState> {
    let marker = directory.join("serve-owned.json");
    if !enabled && (!managed || !marker.exists()) {
        return Ok(None);
    }
    let program = tailscale_program()?;
    native_setup_with_program(
        directory,
        enabled,
        state.store.as_ref(),
        discovered,
        managed,
        &program,
    )
    .await
}

async fn native_setup_with_program(
    directory: &Path,
    enabled: bool,
    store: &dyn plurx_core::store::Store,
    discovered: &mut Option<IpAddr>,
    managed: bool,
    program: &Path,
) -> Result<Option<HostAddress>, SetupState> {
    let marker = directory.join("serve-owned.json");
    if !enabled && (!managed || !marker.exists()) {
        return Ok(None);
    }
    let address = if enabled {
        Some(host_address(
            &command_json(program, &["status", "--json", "--peers=false"]).await?,
        )?)
    } else {
        None
    };
    *discovered = address.as_ref().map(|address| IpAddr::V4(address.ipv4));
    let config = command_json(program, &["serve", "status", "--json"]).await?;
    if !managed {
        return match serve_rule(&config) {
            ServeRule::Ours => Ok(address),
            ServeRule::Missing => Err(SetupState::NativeSetupRequired),
            ServeRule::Conflict => Err(SetupState::PortConflict),
        };
    }
    let owned =
        read_json(&marker).is_ok_and(|v| v == serde_json::json!({"port":PORT,"target":TARGET}));
    match (enabled, serve_rule(&config), owned) {
        (true, ServeRule::Missing, _) => {
            if crate::sharing::enabled(store)
                .await
                .map_err(|_| SetupState::Unavailable)?
                != enabled
            {
                return Err(SetupState::Pending);
            }
            // Persist the finite creation intent before crossing the host
            // boundary so a restart can finish or clean up this exact rule.
            write_json(&marker, &serde_json::json!({"port":PORT,"target":TARGET}))
                .map_err(|_| SetupState::Unavailable)?;
            command(
                program,
                &["serve", "--bg", "--tcp=32443", "tcp://127.0.0.1:32444"],
            )
            .await?;
            if serve_rule(&command_json(program, &["serve", "status", "--json"]).await?)
                != ServeRule::Ours
            {
                return Err(SetupState::Unavailable);
            }
        }
        (true, ServeRule::Conflict, _) | (false, ServeRule::Conflict, true) => {
            return Err(SetupState::PortConflict)
        }
        (false, ServeRule::Ours, true) => {
            if crate::sharing::enabled(store)
                .await
                .map_err(|_| SetupState::Unavailable)?
                != enabled
            {
                return Err(SetupState::Pending);
            }
            command(program, &["serve", "--tcp=32443", "off"]).await?;
            std::fs::remove_file(marker).map_err(|_| SetupState::Unavailable)?;
        }
        (false, ServeRule::Missing, true) => {
            std::fs::remove_file(marker).map_err(|_| SetupState::Unavailable)?;
        }
        _ => {}
    }
    Ok(address)
}
fn external_report(
    directory: &Path,
    generation: &str,
    node: &str,
    enabled: bool,
    now: i64,
    guard: &mut bool,
) -> Result<Option<HostAddress>, SetupState> {
    *guard = false;
    let raw = read_json_owned(&directory.join("report.json"), true)
        .map_err(|_| SetupState::HostHelperRequired)?;
    validate_external_report(raw, generation, node, enabled, now, guard)
}
fn validate_external_report(
    raw: serde_json::Value,
    generation: &str,
    node: &str,
    enabled: bool,
    now: i64,
    guard: &mut bool,
) -> Result<Option<HostAddress>, SetupState> {
    *guard = false;
    let report: HostReport =
        serde_json::from_value(raw).map_err(|_| SetupState::HostHelperRequired)?;
    if report.schema_version != 1
        || report.generation != generation
        || report.node_id != node
        || report.enabled != enabled
        || report.checked_at_ms > now.saturating_add(5000)
        || now.saturating_sub(report.checked_at_ms) > 30_000
    {
        return Err(SetupState::HostHelperRequired);
    }
    if let Some(address) = &report.address {
        address.endpoint("0".repeat(64))?;
    }
    *guard = enabled && report.egress_guard;
    if report.state
        != if enabled {
            SetupState::Ready
        } else {
            SetupState::Disabled
        }
    {
        return Err(report.state);
    }
    if enabled && report.address.is_none() {
        return Err(SetupState::Unavailable);
    }
    if enabled && !report.egress_guard {
        return Err(SetupState::DeploymentRequired);
    }
    Ok(report.address)
}
fn merge_endpoint(
    mut endpoints: Vec<Endpoint>,
    endpoint: Endpoint,
) -> Result<Option<Vec<Endpoint>>, SetupState> {
    if let Some(existing) = endpoints
        .iter_mut()
        .find(|e| e.ts_fqdn == endpoint.ts_fqdn || e.spki_sha256 == endpoint.spki_sha256)
    {
        if existing.port != PORT || *existing == endpoint {
            return Ok(None);
        }
        *existing = endpoint;
    } else if endpoints.len() < 4 {
        endpoints.push(endpoint);
    } else {
        return Ok(None);
    }
    validate_endpoints(&endpoints).map_err(|_| SetupState::Unavailable)?;
    Ok(Some(endpoints))
}
async fn publish_endpoint(state: &AppState, address: &HostAddress) -> Result<(), SetupState> {
    let Some(certificate) = state.sharing.status().certificate else {
        return Err(SetupState::Pending);
    };
    let endpoint = address.endpoint(certificate.spki_sha256)?;
    let manifest = state
        .store
        .sharing_endpoint_manifest()
        .await
        .map_err(|_| SetupState::Unavailable)?;
    let revision = manifest.as_ref().map_or(0, |m| m.revision);
    // A Cinema can advertise four ingress hosts. Preserve that selection on
    // larger clusters; configuring another host does not evict one.
    let Some(endpoints) =
        merge_endpoint(manifest.map_or_else(Vec::new, |m| m.endpoints), endpoint)?
    else {
        return Ok(());
    };
    match state
        .store
        .set_sharing_endpoint_manifest(revision, endpoints)
        .await
        .map_err(|_| SetupState::Unavailable)?
    {
        MutationOutcome::Applied => Ok(()),
        _ => Err(SetupState::Pending),
    }
}
pub(crate) async fn run(state: AppState, shutdown: CancellationToken, native_managed: bool) {
    let directory = state.sharing.host_directory();
    let generation = uuid::Uuid::new_v4().to_string();
    let container = state.sharing.network.listener_profile == SharingListenerProfile::DockerBridge
        || Path::new("/.dockerenv").exists()
        || Path::new("/run/.containerenv").exists();
    #[cfg(target_os = "macos")]
    let native_managed = native_managed
        || (!container
            && plurx_core::process::bounded::cancellable(shutdown.clone(), async {
                Ok::<_, SetupState>(legacy_native_service().await)
            })
            .await
            .unwrap_or(false));
    let mode = HostMode::select(native_managed, container);
    tracing::info!(?mode, "sharing host setup mode selected");
    let mut last_setup = None;
    loop {
        if shutdown.is_cancelled() {
            return;
        }
        if let Ok(enabled) = crate::sharing::enabled(state.store.as_ref()).await {
            let mut discovered = None;
            let mut guard = false;
            let result = if private_directory(&directory).is_err() {
                Err(SetupState::Unavailable)
            } else if mode == HostMode::ExternalContainer {
                let request = HostRequest {
                    schema_version: 1,
                    generation: &generation,
                    node_id: &state.node_id,
                    enabled,
                    checked_at_ms: clock_ms(),
                    listener_profile: if state.sharing.network.listener_profile
                        == SharingListenerProfile::DockerBridge
                    {
                        "docker_bridge"
                    } else {
                        "host_loopback"
                    },
                    listener_address: state.sharing.network.bind.to_string(),
                    container_egress: match state.sharing.network.egress {
                        plurx_core::config::SharingEgressConfig::LocalAddress { address } => {
                            Some(address)
                        }
                        _ => None,
                    },
                };
                if write_json(&directory.join("request.json"), &request).is_err() {
                    Err(SetupState::Unavailable)
                } else {
                    match external_report(
                        &directory,
                        &generation,
                        &state.node_id,
                        enabled,
                        clock_ms(),
                        &mut guard,
                    ) {
                        Err(SetupState::HostHelperRequired) if !enabled => Ok(None),
                        result => result,
                    }
                }
            } else if enabled && state.sharing.network.bind.to_string() != TARGET {
                Err(SetupState::NativeSetupRequired)
            } else {
                // Signal the child owner and await settlement; dropping this
                // future on shutdown would skip its confirmed reap path.
                plurx_core::process::bounded::cancellable(
                    shutdown.clone(),
                    native_setup(
                        &directory,
                        enabled,
                        &state,
                        &mut discovered,
                        mode == HostMode::ManagedNative,
                    ),
                )
                .await
            };
            let (setup, address) = match result {
                Ok(Some(address)) if enabled => match publish_endpoint(&state, &address).await {
                    Ok(()) => (SetupState::Ready, Some(IpAddr::V4(address.ipv4))),
                    Err(error) => (error, Some(IpAddr::V4(address.ipv4))),
                },
                Ok(_) => (SetupState::Disabled, None),
                Err(error) => (error, discovered),
            };
            if last_setup != Some(setup) {
                if matches!(setup, SetupState::Ready | SetupState::Disabled) {
                    tracing::info!(?mode, ?setup, "sharing host setup changed");
                } else {
                    tracing::warn!(?mode, ?setup, "sharing host setup needs attention");
                }
                last_setup = Some(setup);
            }
            state
                .sharing
                .observe_host_setup(setup.status(), address, guard);
        } else {
            state
                .sharing
                .observe_host_setup(SetupState::Unavailable.status(), None, false);
        }
        tokio::select! { () = shutdown.cancelled() => return, () = tokio::time::sleep(POLL) => {} }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn native_host_mode_keeps_unmanaged_hosts_out_of_container_handoff() {
        assert_eq!(HostMode::select(false, false), HostMode::UnmanagedNative);
        assert_eq!(HostMode::select(true, false), HostMode::ManagedNative);
        assert_eq!(HostMode::select(false, true), HostMode::ExternalContainer);
        assert_eq!(HostMode::select(true, true), HostMode::ExternalContainer);
        assert!(!SetupState::NativeSetupRequired
            .status()
            .message
            .contains("Docker"));
    }

    #[cfg(unix)]
    #[test]
    fn legacy_launchd_identity_requires_exact_process_and_owned_registration() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let directory = tempfile::tempdir().expect("fixture");
        let program = directory.path().join("plurxd");
        let registration = directory.path().join("com.plurx.plurxd.plist");
        std::fs::write(&program, b"fixture").expect("program");
        std::fs::write(&registration, b"fixture registration").expect("registration");
        for file in [&program, &registration] {
            std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600))
                .expect("private fixture");
        }
        let uid = std::fs::metadata(&registration).expect("metadata").uid();
        let service = "gui/501/com.plurx.plurxd";
        let fixture = format!(
            "{service} = {{\n\tpath = {}\n\tprogram = {}\n\tpid = 42\n}}\n",
            registration.display(),
            program.display()
        );
        let matches = |output: &str, pid, executable: &Path, owner| {
            legacy_service_matches(
                output.as_bytes(),
                service,
                pid,
                executable,
                &registration,
                owner,
            )
        };
        assert_eq!(matches(&fixture, 42, &program, uid), uid != 0);
        assert!(!matches(&fixture, 43, &program, uid));
        assert!(!matches(&fixture, 42, &registration, uid));
        assert!(!matches(&fixture, 42, &program, uid.saturating_add(1)));
        for bad in [
            fixture.replace("\tpid = 42", "\targuments = {\n\t\tpid = 42\n\t}"),
            fixture.replace("\tpid = 42", "\tpid = 42\n\tpid = 43"),
            fixture.replace("\tpid = 42", "\tpid = invalid"),
            fixture.replace("\tpath = ", "\t\tpath = "),
            fixture.replace("gui/501/", "gui/502/"),
            fixture.trim_end_matches("}\n").to_owned(),
        ] {
            assert!(
                !matches(&bad, 42, &program, uid),
                "accepted malformed registration"
            );
        }
        std::fs::set_permissions(&registration, std::fs::Permissions::from_mode(0o666))
            .expect("unsafe registration");
        assert!(!matches(&fixture, 42, &program, uid));
        std::fs::remove_file(&registration).expect("remove registration");
        std::os::unix::fs::symlink(&program, &registration).expect("symlink registration");
        assert!(!matches(&fixture, 42, &program, uid));
    }

    #[cfg(unix)]
    fn native_cli_fixture(directory: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let program = directory.join("tailscale-fixture");
        std::fs::write(directory.join("serve.json"), b"{}").expect("empty Serve config");
        std::fs::write(
            directory.join("expected.json"),
            br#"{"TCP":{"32443":{"TCPForward":"127.0.0.1:32444"}}}"#,
        )
        .expect("expected private forward");
        std::fs::write(&program, r#"#!/bin/sh
set -eu
root=$(dirname "$0")
printf '%s\n' "$*" >> "$root/calls"
case "$*" in
  'status --json --peers=false') printf '%s' '{"BackendState":"Running","Self":{"DNSName":"cinema.tail123.ts.net.","TailscaleIPs":["100.80.1.2"]}}' ;;
  'serve status --json') cat "$root/serve.json" ;;
  'serve --bg --tcp=32443 tcp://127.0.0.1:32444') cp "$root/expected.json" "$root/serve.json" ;;
  'serve --tcp=32443 off') printf '{}' > "$root/serve.json" ;;
  *) exit 97 ;;
esac
"#).expect("fixture CLI");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700))
            .expect("fixture executable");
        program
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn managed_native_setup_creates_only_fixed_private_forward() {
        use plurx_core::store::{keys, SettingsStore, SqliteStore};
        let directory = tempfile::tempdir().expect("fixture");
        let program = native_cli_fixture(directory.path());
        let store = SqliteStore::open_in_memory().expect("store");
        store
            .put_setting(keys::SHARING_ENABLED, "1")
            .await
            .expect("enable");
        let mut discovered = None;
        let address = native_setup_with_program(
            directory.path(),
            true,
            &store,
            &mut discovered,
            true,
            &program,
        )
        .await
        .expect("automatic private forwarding")
        .expect("host address");
        assert_eq!(address.ipv4, Ipv4Addr::new(100, 80, 1, 2));
        assert_eq!(discovered, Some(IpAddr::V4(address.ipv4)));
        assert!(directory.path().join("serve-owned.json").is_file());
        let calls = std::fs::read_to_string(directory.path().join("calls")).expect("calls");
        assert_eq!(
            calls.lines().collect::<Vec<_>>(),
            vec![
                "status --json --peers=false",
                "serve status --json",
                "serve --bg --tcp=32443 tcp://127.0.0.1:32444",
                "serve status --json",
            ]
        );
        store
            .put_setting(keys::SHARING_ENABLED, "0")
            .await
            .expect("disable");
        native_setup_with_program(
            directory.path(),
            false,
            &store,
            &mut discovered,
            true,
            &program,
        )
        .await
        .expect("owned forwarding cleanup");
        assert!(!directory.path().join("serve-owned.json").exists());
        assert_eq!(
            read_json(&directory.path().join("serve.json")),
            Ok(json!({}))
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unmanaged_native_observation_discovers_egress_without_host_mutation() {
        use plurx_core::store::SqliteStore;
        let directory = tempfile::tempdir().expect("fixture");
        let program = native_cli_fixture(directory.path());
        let store = SqliteStore::open_in_memory().expect("store");
        for (config, expected) in [
            (json!({}), Err(SetupState::NativeSetupRequired)),
            (json!({"TCP":{"32443":{"TCPForward":TARGET}}}), Ok(true)),
            (
                json!({"TCP":{"32443":{"TCPForward":"127.0.0.1:9000"}}}),
                Err(SetupState::PortConflict),
            ),
        ] {
            write_json(&directory.path().join("serve.json"), &config).expect("config");
            let mut discovered = None;
            let result = native_setup_with_program(
                directory.path(),
                true,
                &store,
                &mut discovered,
                false,
                &program,
            )
            .await;
            assert_eq!(result.map(|address| address.is_some()), expected);
            assert_eq!(discovered, Some(IpAddr::V4(Ipv4Addr::new(100, 80, 1, 2))));
            assert_eq!(read_json(&directory.path().join("serve.json")), Ok(config));
            assert!(!directory.path().join("serve-owned.json").exists());
        }
        let calls = std::fs::read_to_string(directory.path().join("calls")).expect("calls");
        assert_eq!(calls.lines().count(), 6);
        assert!(calls
            .lines()
            .all(|line| matches!(line, "status --json --peers=false" | "serve status --json")));
        native_setup_with_program(directory.path(), false, &store, &mut None, false, &program)
            .await
            .expect("disabled observation");
        assert_eq!(
            std::fs::read_to_string(directory.path().join("calls")).expect("calls"),
            calls
        );
    }

    #[cfg(unix)]
    fn assert_reaped(pid_file: &std::path::Path) {
        let pid: i32 = std::fs::read_to_string(pid_file)
            .expect("child pid")
            .trim()
            .parse()
            .expect("pid");
        assert!(pid > 1);
        // SAFETY: signal zero observes only the exact fixture PID.
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            -1,
            "child still exists after settlement"
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn host_command_timeout_reaps_before_returning() {
        let directory = tempfile::tempdir().expect("fixture");
        let pid_file = directory.path().join("child.pid");
        let result = super::command_with_timeout(
            std::path::Path::new("/bin/sh"),
            &[
                "-c",
                "echo $$ > \"$1\"; exec /bin/sleep 300",
                "host-fixture",
                pid_file.to_str().expect("path"),
            ],
            std::time::Duration::from_secs(2),
        )
        .await;
        assert_eq!(result, Err(super::SetupState::Unavailable));
        assert_reaped(&pid_file);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn host_command_cancellation_reaps_before_returning() {
        let directory = tempfile::tempdir().expect("fixture");
        let pid_file = directory.path().join("child.pid");
        let child_path = pid_file.clone();
        let cancel = tokio_util::sync::CancellationToken::new();
        let child_cancel = cancel.clone();
        let task = tokio::spawn(async move {
            plurx_core::process::bounded::cancellable(
                child_cancel,
                super::command(
                    std::path::Path::new("/bin/sh"),
                    &[
                        "-c",
                        "echo $$ > \"$1\"; exec /bin/sleep 300",
                        "host-fixture",
                        child_path.to_str().expect("path"),
                    ],
                ),
            )
            .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if tokio::fs::read_to_string(&pid_file)
                    .await
                    .ok()
                    .and_then(|text| text.trim().parse::<i32>().ok())
                    .is_some_and(|pid| pid > 1)
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("child announced itself");
        cancel.cancel();
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), task)
            .await
            .expect("settled cancellation")
            .expect("joined fixture");
        assert_eq!(result, Err(super::SetupState::Unavailable));
        assert_reaped(&pid_file);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn host_command_preserves_status_and_output_bound() {
        let shell = std::path::Path::new("/bin/sh");
        assert_eq!(
            super::command(shell, &["-c", "printf ready"]).await,
            Ok(b"ready".to_vec())
        );
        assert_eq!(
            super::command(shell, &["-c", "exit 7"]).await,
            Err(super::SetupState::PermissionRequired)
        );
        assert_eq!(
            super::command(shell, &["-c", "yes x | head -c 262145"]).await,
            Err(super::SetupState::Unavailable)
        );
        assert_eq!(
            super::command(shell, &["-c", "yes x | head -c 262145; exit 7"]).await,
            Err(super::SetupState::Unavailable)
        );
    }

    use super::*;
    use serde_json::json;
    #[cfg(unix)]
    #[test]
    fn daemon_owned_reports_cannot_authorize_a_host_guard() {
        assert!(owner_allowed(0, 1000, true));
        assert!(!owner_allowed(1000, 1000, true));
        assert!(owner_allowed(1000, 1000, false));
        // SAFETY: geteuid has no pointer arguments or side effects.
        if unsafe { libc::geteuid() } != 0 {
            let directory = tempfile::tempdir().expect("synthetic sharing host fixture");
            let path = directory.path().join("forged-report.json");
            write_json(&path, &json!({"egress_guard":true}))
                .expect("synthetic sharing host fixture");
            assert!(read_json_owned(&path, true).is_err());
        }
    }
    fn endpoint(index: u8) -> Endpoint {
        HostAddress {
            ipv4: Ipv4Addr::new(100, 80, 1, index),
            ipv6: None,
            ts_fqdn: format!("cinema{index}.tail123.ts.net"),
        }
        .endpoint(format!("{index:064x}"))
        .expect("synthetic sharing host fixture")
    }
    #[test]
    fn automatic_endpoint_publication_preserves_cluster_selection_and_custom_ports() {
        let selected = (1..=4).map(endpoint).collect::<Vec<_>>();
        assert!(merge_endpoint(selected.clone(), endpoint(5))
            .expect("synthetic sharing host fixture")
            .is_none());
        let mut corrected = endpoint(2);
        corrected.spki_sha256 = "a".repeat(64);
        let updated = merge_endpoint(selected.clone(), corrected.clone())
            .expect("synthetic sharing host fixture")
            .expect("synthetic sharing host fixture");
        assert_eq!(updated.len(), 4);
        assert_eq!(updated[1], corrected);
        assert_eq!(updated[0], selected[0]);
        assert_eq!(updated[2..], selected[2..]);
        let mut custom = endpoint(2);
        custom.port = 8443;
        assert!(merge_endpoint(vec![custom], endpoint(2))
            .expect("synthetic sharing host fixture")
            .is_none());
        assert!(merge_endpoint(selected, endpoint(2))
            .expect("synthetic sharing host fixture")
            .is_none());
    }
    #[test]
    fn managed_bridge_has_no_socket_source_before_its_host_guard_receipt() {
        use plurx_core::{
            config::{SharingEgressConfig, SharingNetworkConfig},
            secrets::CredentialKey,
            sharing_tls::PinnedEgress,
        };
        use std::sync::Arc;
        let source: IpAddr = "172.30.44.2"
            .parse()
            .expect("synthetic sharing host fixture");
        let config = SharingNetworkConfig {
            listener_profile: SharingListenerProfile::DockerBridge,
            bind: "0.0.0.0:32444"
                .parse()
                .expect("synthetic sharing host fixture"),
            egress: SharingEgressConfig::LocalAddress { address: source },
            require_host_egress_guard: true,
        };
        let manager = crate::sharing::SharingManager::new(
            Arc::new(CredentialKey::from_bytes([19; 32])),
            PathBuf::from("unused-managed-sharing-fixture"),
            config,
        );
        assert!(matches!(manager.egress(), PinnedEgress::LocalAddress(ip) if ip.is_unspecified()));
        manager.observe_host_setup(SetupState::PortConflict.status(), None, true);
        #[cfg(unix)]
        // SAFETY: geteuid has no pointer arguments or side effects.
        let unprivileged = unsafe { libc::geteuid() } != 0;
        #[cfg(not(unix))]
        let unprivileged = false;
        assert!(
            matches!(manager.egress(), PinnedEgress::LocalAddress(ip) if if unprivileged { ip == source } else { ip.is_unspecified() })
        );
        manager.observe_host_setup(SetupState::HostHelperRequired.status(), None, false);
        assert!(matches!(manager.egress(), PinnedEgress::LocalAddress(ip) if ip.is_unspecified()));
    }
    #[cfg(not(target_os = "linux"))]
    #[test]
    fn native_default_binding_uses_discovered_tailnet_ip_and_preserves_explicit_binding() {
        use plurx_core::{
            config::{SharingEgressConfig, SharingNetworkConfig},
            secrets::CredentialKey,
            sharing_tls::PinnedEgress,
        };
        use std::sync::Arc;
        let discovered = IpAddr::V4(Ipv4Addr::new(100, 80, 1, 2));
        let manager = crate::sharing::SharingManager::new(
            Arc::new(CredentialKey::from_bytes([17; 32])),
            PathBuf::from("unused-native-sharing-fixture"),
            SharingNetworkConfig::default(),
        );
        manager.observe_host_setup(
            SetupState::PermissionRequired.status(),
            Some(discovered),
            false,
        );
        assert!(matches!(manager.egress(), PinnedEgress::LocalAddress(ip) if ip == discovered));
        let explicit = IpAddr::V4(Ipv4Addr::new(100, 80, 1, 3));
        let config = SharingNetworkConfig {
            egress: SharingEgressConfig::LocalAddress { address: explicit },
            ..Default::default()
        };
        let manager = crate::sharing::SharingManager::new(
            Arc::new(CredentialKey::from_bytes([18; 32])),
            PathBuf::from("unused-native-sharing-fixture"),
            config,
        );
        manager.observe_host_setup(SetupState::Ready.status(), Some(discovered), false);
        assert!(matches!(manager.egress(), PinnedEgress::LocalAddress(ip) if ip == explicit));
    }
    #[test]
    fn host_identity_accepts_only_private_tailnet_addresses() {
        let fixture = |ip: &str| json!({"BackendState":"Running","Self":{"DNSName":"cinema.tail123.ts.net.","TailscaleIPs":[ip]}});
        assert_eq!(
            host_address(&fixture("100.80.1.2"))
                .expect("synthetic sharing host fixture")
                .ts_fqdn,
            "cinema.tail123.ts.net"
        );
        assert!(host_address(&fixture("192.168.1.2")).is_err());
        assert!(host_address(&json!({"BackendState":"NeedsLogin"})).is_err());
    }
    #[test]
    fn sharing_setup_never_replaces_other_services_or_public_rules() {
        assert_eq!(
            serve_rule(&json!({"TCP":{"443":{"HTTPS":true}}})),
            ServeRule::Missing
        );
        assert_eq!(
            serve_rule(&json!({"Foreground":{"unrelated":{"TCP":{"443":{"HTTPS":true}}}}})),
            ServeRule::Missing
        );
        assert_eq!(
            serve_rule(&json!({"TCP":{"32443":{"TCPForward":TARGET}}})),
            ServeRule::Ours
        );
        assert_eq!(
            serve_rule(&json!({"TCP":{"32443":{"TCPForward":"127.0.0.1:9999"}}})),
            ServeRule::Conflict
        );
        assert_eq!(
            serve_rule(
                &json!({"TCP":{"32443":{"TCPForward":TARGET}},"AllowFunnel":{"cinema.tail123.ts.net:32443":true}})
            ),
            ServeRule::Conflict
        );
    }
    #[test]
    fn docker_observations_are_fresh_and_bound_to_the_addressed_process() {
        let directory = tempfile::tempdir().expect("synthetic sharing host fixture");
        let path = directory.path().join("report.json");
        let mut report = json!({"schema_version":1,"generation":"current","node_id":"node","enabled":true,"checked_at_ms":100000,"state":"ready","egress_guard":true,"address":{"ipv4":"100.80.1.2","ipv6":null,"ts_fqdn":"cinema.tail123.ts.net"}});
        write_json(&path, &report).expect("synthetic sharing host fixture");
        let mut guard = false;
        assert!(validate_external_report(
            read_json(&path).expect("synthetic sharing host fixture"),
            "current",
            "node",
            true,
            100001,
            &mut guard
        )
        .is_ok());
        assert!(guard);
        assert!(validate_external_report(
            read_json(&path).expect("synthetic sharing host fixture"),
            "old",
            "node",
            true,
            100001,
            &mut guard
        )
        .is_err());
        assert!(!guard);
        assert!(validate_external_report(
            read_json(&path).expect("synthetic sharing host fixture"),
            "current",
            "other",
            true,
            100001,
            &mut guard
        )
        .is_err());
        assert!(validate_external_report(
            read_json(&path).expect("synthetic sharing host fixture"),
            "current",
            "node",
            false,
            100001,
            &mut guard
        )
        .is_err());
        assert!(validate_external_report(
            read_json(&path).expect("synthetic sharing host fixture"),
            "current",
            "node",
            true,
            130001,
            &mut guard
        )
        .is_err());
        report["address"]["ipv4"] = json!("1.1.1.1");
        write_json(&path, &report).expect("synthetic sharing host fixture");
        assert!(validate_external_report(
            read_json(&path).expect("synthetic sharing host fixture"),
            "current",
            "node",
            true,
            100001,
            &mut guard
        )
        .is_err());
        assert!(!guard);
    }
}
