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
    process::Stdio,
    time::Duration,
};
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

const LIMIT: usize = 256 * 1024;
const TARGET: &str = "127.0.0.1:32444";
const PORT: u16 = 32443;
const POLL: Duration = Duration::from_secs(5);

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

fn private_directory(path: &Path) -> std::io::Result<()> {
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
fn write_json(path: &Path, value: &impl Serialize) -> std::io::Result<()> {
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
fn read_json(path: &Path) -> Result<serde_json::Value, SetupState> {
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
    let mut process = tokio::process::Command::new(program);
    process
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let (mut child, _job) = crate::process_control::spawn_job_owned(
        &mut process,
        crate::process_control::ChildWork::background("private sharing host setup"),
    )
    .map_err(|_| SetupState::PermissionRequired)?;
    let result = tokio::time::timeout(Duration::from_secs(8), async {
        let mut bytes = Vec::new();
        child
            .stdout
            .take()
            .ok_or(SetupState::Unavailable)?
            .take((LIMIT + 1) as u64)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| SetupState::Unavailable)?;
        if bytes.len() > LIMIT {
            return Err(SetupState::Unavailable);
        }
        if !child
            .wait()
            .await
            .map_err(|_| SetupState::Unavailable)?
            .success()
        {
            return Err(SetupState::PermissionRequired);
        }
        Ok(bytes)
    })
    .await
    .map_err(|_| SetupState::Unavailable)?;
    if result.is_err() {
        let _ = child.kill().await;
    }
    result
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
) -> Result<Option<HostAddress>, SetupState> {
    let marker = directory.join("serve-owned.json");
    if !enabled && !marker.exists() {
        return Ok(None);
    }
    let program = tailscale_program()?;
    let address = if enabled {
        Some(host_address(
            &command_json(&program, &["status", "--json", "--peers=false"]).await?,
        )?)
    } else {
        None
    };
    *discovered = address.as_ref().map(|address| IpAddr::V4(address.ipv4));
    let config = command_json(&program, &["serve", "status", "--json"]).await?;
    let owned =
        read_json(&marker).is_ok_and(|v| v == serde_json::json!({"port":PORT,"target":TARGET}));
    match (enabled, serve_rule(&config), owned) {
        (true, ServeRule::Missing, _) => {
            if crate::sharing::enabled(state.store.as_ref())
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
                &program,
                &["serve", "--bg", "--tcp=32443", "tcp://127.0.0.1:32444"],
            )
            .await?;
            if serve_rule(&command_json(&program, &["serve", "status", "--json"]).await?)
                != ServeRule::Ours
            {
                return Err(SetupState::Unavailable);
            }
        }
        (true, ServeRule::Conflict, _) | (false, ServeRule::Conflict, true) => {
            return Err(SetupState::PortConflict)
        }
        (false, ServeRule::Ours, true) => {
            if crate::sharing::enabled(state.store.as_ref())
                .await
                .map_err(|_| SetupState::Unavailable)?
                != enabled
            {
                return Err(SetupState::Pending);
            }
            command(&program, &["serve", "--tcp=32443", "off"]).await?;
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
    let external = !native_managed
        || state.sharing.network.listener_profile == SharingListenerProfile::DockerBridge
        || Path::new("/.dockerenv").exists()
        || Path::new("/run/.containerenv").exists();
    loop {
        if shutdown.is_cancelled() {
            return;
        }
        if let Ok(enabled) = crate::sharing::enabled(state.store.as_ref()).await {
            let mut discovered = None;
            let mut guard = false;
            let result = if private_directory(&directory).is_err() {
                Err(SetupState::Unavailable)
            } else if external {
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
                Err(SetupState::DeploymentRequired)
            } else {
                tokio::select! {
                    () = shutdown.cancelled() => return,
                    result = native_setup(&directory, enabled, &state, &mut discovered) => result,
                }
            };
            let (setup, address) = match result {
                Ok(Some(address)) if enabled => match publish_endpoint(&state, &address).await {
                    Ok(()) => (SetupState::Ready, Some(IpAddr::V4(address.ipv4))),
                    Err(error) => (error, Some(IpAddr::V4(address.ipv4))),
                },
                Ok(_) => (SetupState::Disabled, None),
                Err(error) => (error, discovered),
            };
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
