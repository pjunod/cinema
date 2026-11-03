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
    /// The concrete step that failed, in the host tool's own words where it
    /// gave any. Empty when the state needs no further explanation.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub detail: String,
    pub checked_at_ms: Option<i64>,
    /// Fresh control-plane observation, separate from forwarding readiness.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tailscale_online: Option<bool>,
    /// Which Tailscale account and tailnet this host is signed in to, when
    /// the host could be observed. Another owner's device share must be
    /// accepted with this account for this Cinema to reach their server.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tailscale: Option<TailscaleIdentity>,
}
/// This host's own Tailscale identity, as reported by its local CLI.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TailscaleIdentity {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tailnet: Option<String>,
    pub dns_name: String,
    pub ipv4: Ipv4Addr,
}
/// A failed setup observation and the concrete step that failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Failure {
    pub state: SetupState,
    pub detail: String,
}
impl From<SetupState> for Failure {
    fn from(state: SetupState) -> Self {
        Self {
            state,
            detail: String::new(),
        }
    }
}
impl Failure {
    fn new(state: SetupState, detail: impl Into<String>) -> Self {
        Self {
            state,
            detail: detail.into(),
        }
    }
}
type Setup<T> = Result<T, Failure>;
/// Host tool output shown to an administrator: one bounded printable line,
/// with links removed so a sign-in URL never reaches a log or a page.
fn excerpt(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    let mut out = String::new();
    for word in line.split_whitespace() {
        let word = if word.contains("http://") || word.contains("https://") {
            "<link>"
        } else {
            word
        };
        if !out.is_empty() {
            out.push(' ');
        }
        out.extend(word.chars().filter(|c| !c.is_control()));
        if out.chars().count() > 240 {
            out = out.chars().take(240).collect::<String>() + "…";
            break;
        }
    }
    out
}
fn bounded_label(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|v| !v.is_empty() && v.len() <= 256 && !v.chars().any(char::is_control))
        .map(str::to_owned)
}
impl SetupState {
    fn status(self) -> SetupStatus {
        SetupStatus { state: self, detail: String::new(), checked_at_ms: Some(clock_ms()), tailscale_online: None, tailscale: None, message: match self {
            Self::Pending => "Setting up the private connection…",
            Self::Disabled => "Sharing is disabled.",
            Self::Ready => "Private host setup is configured. Pair another Cinema to verify the connection.",
            Self::TailscaleMissing => "Install Tailscale on this Cinema's host, then sign in. Cinema will retry automatically.",
            Self::SignInRequired => "Open Tailscale on this Cinema's host and sign in. Cinema will retry automatically.",
            Self::PermissionRequired => "Allow Cinema's host setup component to configure Tailscale, then retry.",
            Self::PortConflict => "Another Tailscale rule uses Cinema's private port. Cinema has preserved that rule; resolve the conflict on this host.",
            Self::HostHelperRequired => "Cinema runs in a container and cannot see this host's Tailscale. Install Cinema's sharing host component on the Docker host for automatic setup and status; forwarding you configured by hand may still work, and other Cinemas' connection checks show whether it does.",
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tailscale_online: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    login_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tailnet: Option<String>,
}
impl HostAddress {
    pub(crate) fn identity(&self) -> TailscaleIdentity {
        // A Docker host report is another process's file: bound it here too.
        TailscaleIdentity {
            login_name: bounded_label(self.login_name.as_deref()),
            tailnet: bounded_label(self.tailnet.as_deref()),
            dns_name: self.ts_fqdn.clone(),
            ipv4: self.ipv4,
        }
    }
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
/// Run the host's Tailscale CLI.
///
/// `TAILSCALE_BE_CLI=1` is required on macOS: the Tailscale app's binary
/// decides from its launch context whether it is the CLI or the menu-bar app.
/// Started by a process that launchd owns (the Cinema LaunchAgent), it takes
/// the GUI path, prints "The Tailscale GUI failed to start" on stdout and
/// exits 0, so every observation failed as unreadable output. The variable
/// is the app's documented CLI switch and is ignored by other platforms' CLI.
async fn command(program: &Path, args: &[&str]) -> Setup<Vec<u8>> {
    command_with_env(
        program,
        args,
        Duration::from_secs(8),
        TAILSCALE_CLI,
        LIMIT,
        SetupState::Unavailable,
    )
    .await
}
/// A Tailscale change (`serve`). Its unexplained refusal is the host denying
/// Cinema the change, as before; read-only observations do not assume that.
async fn command_mutation(program: &Path, args: &[&str]) -> Setup<Vec<u8>> {
    command_with_env(
        program,
        args,
        Duration::from_secs(8),
        TAILSCALE_CLI,
        LIMIT,
        SetupState::PermissionRequired,
    )
    .await
}
const TAILSCALE_CLI: &[(&str, &str)] = &[("TAILSCALE_BE_CLI", "1")];

#[cfg(any(target_os = "macos", test))]
async fn command_with_timeout(program: &Path, args: &[&str], deadline: Duration) -> Setup<Vec<u8>> {
    command_with_env(program, args, deadline, &[], LIMIT, SetupState::Unavailable).await
}

fn invocation(program: &Path, args: &[&str]) -> String {
    let name = program
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| {
            if name.eq_ignore_ascii_case("tailscale") || name.eq_ignore_ascii_case("tailscale.exe")
            {
                "tailscale"
            } else {
                name
            }
        })
        .unwrap_or("host tool");
    std::iter::once(name)
        .chain(args.iter().copied())
        .collect::<Vec<_>>()
        .join(" ")
}

async fn command_with_env(
    program: &Path,
    args: &[&str],
    deadline: Duration,
    env: &[(&str, &str)],
    max_output: usize,
    refusal: SetupState,
) -> Setup<Vec<u8>> {
    let mut process = tokio::process::Command::new(program);
    process.args(args);
    for (key, value) in env {
        process.env(key, value);
    }
    // Bounded and link-free like the tool's own words: the fixed Tailscale
    // arguments carry nothing secret, and a test or future caller's may.
    let shown = excerpt(invocation(program, args).as_bytes());
    // Host tooling keeps its existing user/IPC environment. The shared owner
    // isolates its process group and reaps it before timeout/cancellation returns.
    let output = plurx_core::process::bounded::output_command_inherited_env(
        &mut process,
        deadline,
        max_output + 1,
        crate::process_control::ChildWork::background("private sharing host setup"),
    )
    .await
    .map_err(|error| match error.kind() {
        std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::NotFound => Failure::new(
            SetupState::PermissionRequired,
            format!("`{shown}` could not be started ({}).", error.kind()),
        ),
        std::io::ErrorKind::TimedOut => Failure::new(
            SetupState::Unavailable,
            format!("`{shown}` did not answer within {} s.", deadline.as_secs()),
        ),
        kind => Failure::new(
            SetupState::Unavailable,
            format!("`{shown}` could not be run ({kind})."),
        ),
    })?;
    if output.stdout.len() > max_output {
        return Err(Failure::new(
            SetupState::Unavailable,
            format!("`{shown}` printed more output than Cinema accepts."),
        ));
    }
    if !output.status.success() {
        let said = excerpt(if output.stderr.is_empty() {
            &output.stdout
        } else {
            &output.stderr
        });
        // Classify on everything the tool said, not only the line shown.
        let lower = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase()
            + "\n"
            + &String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();
        let state = if lower.contains("access denied")
            || lower.contains("permission denied")
            || lower.contains("not permitted")
            || lower.contains("operator")
        {
            SetupState::PermissionRequired
        } else if lower.contains("logged out")
            || lower.contains("not logged in")
            || lower.contains("needslogin")
            || lower.contains("log in")
        {
            SetupState::SignInRequired
        } else {
            refusal
        };
        let status = output
            .status
            .code()
            .map_or_else(|| "a signal".to_owned(), |code| format!("status {code}"));
        return Err(Failure::new(
            state,
            if said.is_empty() {
                format!("`{shown}` exited with {status}.")
            } else {
                format!("`{shown}` exited with {status}: {said}")
            },
        ));
    }
    Ok(output.stdout)
}
async fn command_json(program: &Path, args: &[&str]) -> Setup<serde_json::Value> {
    let output = command(program, args).await?;
    serde_json::from_slice(&output).map_err(|_| {
        let said = excerpt(&output);
        Failure::new(
            SetupState::Unavailable,
            if said.is_empty() {
                format!(
                    "`{}` printed nothing Cinema could read.",
                    invocation(program, args)
                )
            } else {
                format!(
                    "`{}` did not answer with status information. It printed: {said}",
                    invocation(program, args)
                )
            },
        )
    })
}
fn host_address(status: &serde_json::Value) -> Setup<HostAddress> {
    if status["BackendState"] != "Running" {
        let backend = bounded_label(status["BackendState"].as_str());
        return Err(Failure::new(
            if backend.as_deref() == Some("Stopped") {
                SetupState::Unavailable
            } else {
                SetupState::SignInRequired
            },
            match backend {
                Some(backend) => format!("Tailscale reports its state as {backend}, not Running."),
                None => "Tailscale did not report a running state.".to_owned(),
            },
        ));
    }
    let self_node = &status["Self"];
    let ips: Vec<IpAddr> =
        serde_json::from_value(self_node["TailscaleIPs"].clone()).map_err(|_| {
            Failure::new(
                SetupState::Unavailable,
                "Tailscale reported no addresses for this machine.",
            )
        })?;
    let ipv4 = ips
        .iter()
        .find_map(|ip| match ip {
            IpAddr::V4(ip) if is_tailnet_address((*ip).into()) => Some(*ip),
            _ => None,
        })
        .ok_or_else(|| {
            Failure::new(
                SetupState::Unavailable,
                "Tailscale reported no private 100.x IPv4 address for this machine.",
            )
        })?;
    let ipv6 = ips.iter().find_map(|ip| match ip {
        IpAddr::V6(ip) if is_tailnet_address((*ip).into()) => Some(*ip),
        _ => None,
    });
    let ts_fqdn = self_node["DNSName"]
        .as_str()
        .ok_or_else(|| {
            Failure::new(
                SetupState::Unavailable,
                "Tailscale reported no machine name (MagicDNS name) for this machine.",
            )
        })?
        .trim_end_matches('.')
        .to_owned();
    // The signed-in account. Personal tailnets are named after the account;
    // a device share must be accepted by exactly this login to reach a peer.
    let login_name = self_node["UserID"]
        .as_u64()
        .map(|id| id.to_string())
        .or_else(|| self_node["UserID"].as_str().map(str::to_owned))
        .and_then(|id| bounded_label(status["User"][id.as_str()]["LoginName"].as_str()));
    let tailnet = bounded_label(status["CurrentTailnet"]["MagicDNSSuffix"].as_str())
        .or_else(|| bounded_label(status["CurrentTailnet"]["Name"].as_str()));
    let address = HostAddress {
        ipv4,
        ipv6,
        ts_fqdn,
        tailscale_online: self_node.get("Online").and_then(serde_json::Value::as_bool),
        login_name,
        tailnet,
    };
    address.endpoint("0".repeat(64)).map_err(|state| {
        Failure::new(
            state,
            format!(
                "Tailscale's machine name {} is not a private Tailnet name Cinema can publish.",
                address.ts_fqdn
            ),
        )
    })?;
    Ok(address)
}
const SWITCH_UNREADABLE: &str = "The saved sharing switch could not be read from the store.";
const MARKER_UNWRITABLE: &str =
    "Cinema could not update its record of the private forward it owns in its data directory.";
fn conflict(config: &serde_json::Value) -> Failure {
    let current = config
        .get("TCP")
        .and_then(|v| v.get(PORT.to_string()))
        .and_then(|rule| rule.get("TCPForward"))
        .and_then(serde_json::Value::as_str)
        .and_then(|target| bounded_label(Some(target)));
    Failure::new(
        SetupState::PortConflict,
        match current {
            Some(target) => format!("Tailscale Serve already forwards port {PORT} to {target}, not to Cinema's listener at {TARGET}."),
            None => format!("Port {PORT} already has a Tailscale Serve, foreground or Funnel rule that Cinema does not own."),
        },
    )
}
/// A peer this host's Tailscale can currently reach, from its own netmap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VisiblePeer {
    pub dns_name: String,
    pub online: Option<bool>,
    /// False for a sharee node: the other owner's device, present only so it
    /// can connect in after this host was shared to them. It cannot be
    /// dialled (observed on a real cross-account pair: the source appeared
    /// as `device-of-shared-to-user` with `ShareeNode: true`).
    pub dialable: bool,
}
/// The peers this native host's Tailscale can see, keyed by Tailscale IP.
/// `None` means the host could not be observed (a container, a missing CLI,
/// or Tailscale not running), never that no peer is visible.
pub(crate) async fn visible_peers() -> Option<std::collections::HashMap<IpAddr, VisiblePeer>> {
    let program = tailscale_program().ok()?;
    let output = command_with_env(
        &program,
        &["status", "--json"],
        Duration::from_secs(3),
        TAILSCALE_CLI,
        // A full netmap runs to about 1.5 KiB a peer; large tailnets must
        // not silently lose the visibility answer.
        8 * 1024 * 1024,
        SetupState::Unavailable,
    )
    .await
    .ok()?;
    parse_visible_peers(&serde_json::from_slice(&output).ok()?)
}
fn parse_visible_peers(
    status: &serde_json::Value,
) -> Option<std::collections::HashMap<IpAddr, VisiblePeer>> {
    if status["BackendState"] != "Running" {
        return None;
    }
    let mut visible = std::collections::HashMap::new();
    let peers = match &status["Peer"] {
        serde_json::Value::Null => return Some(visible),
        serde_json::Value::Object(peers) => peers,
        _ => return None,
    };
    for peer in peers.values() {
        let Ok(ips) = serde_json::from_value::<Vec<IpAddr>>(peer["TailscaleIPs"].clone()) else {
            continue;
        };
        let row = VisiblePeer {
            dns_name: bounded_label(peer["DNSName"].as_str().map(|n| n.trim_end_matches('.')))
                .unwrap_or_default(),
            online: peer["Online"].as_bool(),
            dialable: peer["ShareeNode"] != true,
        };
        for ip in ips {
            // A dialable entry for the same address always wins.
            match visible.get(&ip) {
                Some(VisiblePeer { dialable: true, .. }) => {}
                _ => {
                    visible.insert(ip, row.clone());
                }
            }
        }
    }
    Some(visible)
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
    discovered: &mut Option<HostAddress>,
    managed: bool,
) -> Setup<Option<HostAddress>> {
    let marker = directory.join("serve-owned.json");
    if !enabled && (!managed || !marker.exists()) {
        return Ok(None);
    }
    let program = tailscale_program().map_err(|state| {
        Failure::new(
            state,
            "No Tailscale command-line tool was found in its standard install locations.",
        )
    })?;
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
    discovered: &mut Option<HostAddress>,
    managed: bool,
    program: &Path,
) -> Setup<Option<HostAddress>> {
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
    discovered.clone_from(&address);
    let config = command_json(program, &["serve", "status", "--json"]).await?;
    if !managed {
        return match serve_rule(&config) {
            ServeRule::Ours => Ok(address),
            ServeRule::Missing => Err(Failure::new(
                SetupState::NativeSetupRequired,
                format!("Tailscale Serve has no rule for port {PORT}. This Cinema was not started by its installed service, so it does not change host settings; on this machine run: tailscale serve --bg --tcp={PORT} tcp://{TARGET}"),
            )),
            ServeRule::Conflict => Err(conflict(&config)),
        };
    }
    let owned =
        read_json(&marker).is_ok_and(|v| v == serde_json::json!({"port":PORT,"target":TARGET}));
    match (enabled, serve_rule(&config), owned) {
        (true, ServeRule::Missing, _) => {
            if crate::sharing::enabled(store)
                .await
                .map_err(|_| Failure::new(SetupState::Unavailable, SWITCH_UNREADABLE))?
                != enabled
            {
                return Err(SetupState::Pending.into());
            }
            // Persist the finite creation intent before crossing the host
            // boundary so a restart can finish or clean up this exact rule.
            write_json(&marker, &serde_json::json!({"port":PORT,"target":TARGET}))
                .map_err(|_| Failure::new(SetupState::Unavailable, MARKER_UNWRITABLE))?;
            command_mutation(
                program,
                &["serve", "--bg", "--tcp=32443", "tcp://127.0.0.1:32444"],
            )
            .await?;
            if serve_rule(&command_json(program, &["serve", "status", "--json"]).await?)
                != ServeRule::Ours
            {
                return Err(Failure::new(
                    SetupState::Unavailable,
                    format!("Tailscale accepted the private forward for port {PORT}, but its Serve status does not show it."),
                ));
            }
        }
        (true, ServeRule::Conflict, _) | (false, ServeRule::Conflict, true) => {
            return Err(conflict(&config))
        }
        (false, ServeRule::Ours, true) => {
            if crate::sharing::enabled(store)
                .await
                .map_err(|_| Failure::new(SetupState::Unavailable, SWITCH_UNREADABLE))?
                != enabled
            {
                return Err(SetupState::Pending.into());
            }
            command_mutation(program, &["serve", "--tcp=32443", "off"]).await?;
            std::fs::remove_file(marker)
                .map_err(|_| Failure::new(SetupState::Unavailable, MARKER_UNWRITABLE))?;
        }
        (false, ServeRule::Missing, true) => {
            std::fs::remove_file(marker)
                .map_err(|_| Failure::new(SetupState::Unavailable, MARKER_UNWRITABLE))?;
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
) -> Setup<Option<HostAddress>> {
    *guard = false;
    let raw = read_json_owned(&directory.join("report.json"), true).map_err(|_| {
        Failure::new(
            SetupState::HostHelperRequired,
            "There is no current report from Cinema's sharing host component (sharing-host/report.json in Cinema's data directory).",
        )
    })?;
    validate_external_report(raw, generation, node, enabled, now, guard)
}
fn validate_external_report(
    raw: serde_json::Value,
    generation: &str,
    node: &str,
    enabled: bool,
    now: i64,
    guard: &mut bool,
) -> Setup<Option<HostAddress>> {
    *guard = false;
    let helper = |detail: &str| Failure::new(SetupState::HostHelperRequired, detail);
    let report: HostReport = serde_json::from_value(raw).map_err(|_| {
        helper("The sharing host component's report could not be read; update the component.")
    })?;
    if report.schema_version != 1 {
        return Err(helper("The sharing host component writes a report version this Cinema does not read; update the component."));
    }
    if report.generation != generation || report.node_id != node {
        return Err(helper("The sharing host component has not yet answered this Cinema process; it answers within a few seconds of a restart."));
    }
    if report.enabled != enabled {
        return Err(helper(
            "The sharing host component has not yet seen the current sharing switch.",
        ));
    }
    if report.checked_at_ms > now.saturating_add(5000)
        || now.saturating_sub(report.checked_at_ms) > 30_000
    {
        return Err(helper("The sharing host component's last report is more than 30 seconds old; check that its service is running on the host."));
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
        return Err(Failure::new(
            report.state,
            "Reported by Cinema's sharing host component.",
        ));
    }
    if enabled && report.address.is_none() {
        return Err(Failure::new(
            SetupState::Unavailable,
            "The sharing host component reported no Tailscale address for this host.",
        ));
    }
    if enabled && !report.egress_guard {
        return Err(Failure::new(
            SetupState::DeploymentRequired,
            "The sharing host component has not established the private outbound guard for this container.",
        ));
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
async fn publish_endpoint(state: &AppState, address: &HostAddress) -> Setup<()> {
    let Some(certificate) = state.sharing.status().certificate else {
        return Err(Failure::new(
            SetupState::Pending,
            "Waiting for this node's sharing listener and certificate to start.",
        ));
    };
    let endpoint = address.endpoint(certificate.spki_sha256)?;
    let manifest = state.store.sharing_endpoint_manifest().await.map_err(|_| {
        Failure::new(
            SetupState::Unavailable,
            "This Cinema's published address list could not be read from the store.",
        )
    })?;
    let revision = manifest.as_ref().map_or(0, |m| m.revision);
    // A Cinema can advertise four ingress hosts. Preserve that selection on
    // larger clusters; configuring another host does not evict one.
    let Some(endpoints) = merge_endpoint(manifest.map_or_else(Vec::new, |m| m.endpoints), endpoint)
        .map_err(|state| {
            Failure::new(
                state,
                "This node's address could not be added to the published address list.",
            )
        })?
    else {
        return Ok(());
    };
    match state
        .store
        .set_sharing_endpoint_manifest(revision, endpoints)
        .await
        .map_err(|_| {
            Failure::new(
                SetupState::Unavailable,
                "This node's address could not be saved to the published address list.",
            )
        })? {
        MutationOutcome::Applied => {
            tracing::info!(
                ts_fqdn = %address.ts_fqdn,
                ipv4 = %address.ipv4,
                "sharing published this node's private address"
            );
            Ok(())
        }
        _ => Err(Failure::new(
            SetupState::Pending,
            "Another node changed the published address list at the same time; retrying.",
        )),
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
    state
        .sharing
        .observe_native_host(mode != HostMode::ExternalContainer);
    let mut last_setup: Option<(SetupState, String)> = None;
    loop {
        if shutdown.is_cancelled() {
            return;
        }
        if let Ok(enabled) = crate::sharing::enabled(state.store.as_ref()).await {
            let mut discovered = None;
            let mut guard = false;
            let result: Setup<Option<HostAddress>> = if private_directory(&directory).is_err() {
                Err(Failure::new(
                    SetupState::Unavailable,
                    "Cinema's private sharing-host directory is missing or has unsafe permissions.",
                ))
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
                    Err(Failure::new(
                        SetupState::Unavailable,
                        "Cinema could not write its request for the sharing host component.",
                    ))
                } else {
                    match external_report(
                        &directory,
                        &generation,
                        &state.node_id,
                        enabled,
                        clock_ms(),
                        &mut guard,
                    ) {
                        Err(Failure {
                            state: SetupState::HostHelperRequired,
                            ..
                        }) if !enabled => Ok(None),
                        result => result,
                    }
                }
            } else if enabled && state.sharing.network.bind.to_string() != TARGET {
                Err(Failure::new(
                    SetupState::NativeSetupRequired,
                    format!(
                        "Cinema's sharing listener is configured at {}; automatic native setup forwards only to {TARGET}.",
                        state.sharing.network.bind
                    ),
                ))
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
            let observed = match &result {
                Ok(Some(address)) => Some(address.clone()),
                _ => discovered.clone(),
            };
            let (setup, detail, address) = match result {
                Ok(Some(address)) if enabled => match publish_endpoint(&state, &address).await {
                    Ok(()) => (SetupState::Ready, String::new(), Some(address)),
                    Err(error) => (error.state, error.detail, Some(address)),
                },
                Ok(_) => (SetupState::Disabled, String::new(), None),
                Err(error) => (error.state, error.detail, discovered),
            };
            let changed = last_setup
                .as_ref()
                .is_none_or(|(state, text)| *state != setup || *text != detail);
            if changed {
                let account = observed
                    .as_ref()
                    .and_then(|a| a.identity().login_name)
                    .unwrap_or_default();
                if matches!(setup, SetupState::Ready | SetupState::Disabled) {
                    tracing::info!(?mode, ?setup, %account, "sharing host setup changed");
                } else {
                    tracing::warn!(?mode, ?setup, %detail, %account, "sharing host setup needs attention");
                }
                last_setup = Some((setup, detail.clone()));
            }
            let mut observation = setup.status();
            observation.detail = detail;
            observation.tailscale_online = observed.as_ref().and_then(|a| a.tailscale_online);
            observation.tailscale = observed.as_ref().map(HostAddress::identity);
            state.sharing.observe_host_setup(
                observation,
                address.map(|address| IpAddr::V4(address.ipv4)),
                guard,
            );
        } else {
            let mut observation = SetupState::Unavailable.status();
            observation.detail = SWITCH_UNREADABLE.into();
            state.sharing.observe_host_setup(observation, None, false);
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
# The macOS Tailscale app answers as its menu-bar app, on stdout and with a
# zero exit, unless the CLI switch is set; Cinema runs it from launchd.
if [ "${TAILSCALE_BE_CLI:-}" != 1 ]; then
  printf '%s\n' 'The Tailscale GUI failed to start: The operation could not be completed. (Tailscale.CLIError error 3.)'
  exit 0
fi
printf '%s\n' "$*" >> "$root/calls"
case "$*" in
  'status --json --peers=false') printf '%s' '{"BackendState":"Running","Self":{"DNSName":"cinema.tail123.ts.net.","TailscaleIPs":["100.80.1.2"],"UserID":42},"User":{"42":{"LoginName":"owner@example.com"}},"CurrentTailnet":{"Name":"owner@example.com","MagicDNSSuffix":"tail123.ts.net"}}' ;;
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
        assert_eq!(discovered.as_ref().map(|a| a.ipv4), Some(address.ipv4));
        let identity = address.identity();
        assert_eq!(identity.login_name.as_deref(), Some("owner@example.com"));
        assert_eq!(identity.tailnet.as_deref(), Some("tail123.ts.net"));
        assert_eq!(identity.dns_name, "cinema.tail123.ts.net");
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
            assert_eq!(
                result.map(|address| address.is_some()).map_err(|f| f.state),
                expected
            );
            assert_eq!(
                discovered.as_ref().map(|a| a.ipv4),
                Some(Ipv4Addr::new(100, 80, 1, 2))
            );
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
        assert_eq!(
            result.map_err(|f| f.state),
            Err(super::SetupState::Unavailable)
        );
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
        assert_eq!(
            result.map_err(|f| f.state),
            Err(super::SetupState::Unavailable)
        );
        assert_reaped(&pid_file);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn host_command_preserves_status_and_output_bound() {
        let shell = std::path::Path::new("/bin/sh");
        let state = |r: super::Setup<Vec<u8>>| r.map_err(|f| f.state);
        assert_eq!(
            state(super::command(shell, &["-c", "printf ready"]).await),
            Ok(b"ready".to_vec())
        );
        // A failed command reports what the tool said; only its own words
        // about permission or sign-in choose those states.
        let failed = super::command(
            shell,
            &[
                "-c",
                "echo 'serve config denied: Access denied' >&2; exit 7",
            ],
        )
        .await
        .expect_err("nonzero exit");
        assert_eq!(failed.state, super::SetupState::PermissionRequired);
        assert!(failed.detail.contains("status 7"), "{}", failed.detail);
        assert!(failed.detail.contains("Access denied"), "{}", failed.detail);
        let failed = super::command(shell, &["-c", "echo 'To authenticate, visit: https://login.example/a/secret' >&2; echo 'Logged out.' >&2; exit 1"])
            .await
            .expect_err("nonzero exit");
        assert!(
            !failed.detail.contains("secret"),
            "links never reach a page or log: {}",
            failed.detail
        );
        assert_eq!(
            state(super::command(shell, &["-c", "exit 7"]).await),
            Err(super::SetupState::Unavailable)
        );
        assert_eq!(
            state(super::command(shell, &["-c", "yes x | head -c 262145"]).await),
            Err(super::SetupState::Unavailable)
        );
        assert_eq!(
            state(super::command(shell, &["-c", "yes x | head -c 262145; exit 7"]).await),
            Err(super::SetupState::Unavailable)
        );
        // An unexplained refusal of a Serve change keeps meaning the host
        // denied Cinema the change; a read-only observation does not.
        assert_eq!(
            super::command_mutation(shell, &["-c", "echo 'serve: request failed' >&2; exit 1"])
                .await
                .map_err(|f| f.state),
            Err(super::SetupState::PermissionRequired)
        );
        // Classification reads all of stderr, not only the line shown.
        assert_eq!(
            state(
                super::command(
                    shell,
                    &[
                        "-c",
                        "printf 'error\\nAccess denied: serve config\\n' >&2; exit 1"
                    ]
                )
                .await
            ),
            Err(super::SetupState::PermissionRequired)
        );
        // The macOS CLI switch reaches every Tailscale invocation.
        assert_eq!(
            state(super::command(shell, &["-c", "printf %s \"$TAILSCALE_BE_CLI\""]).await),
            Ok(b"1".to_vec())
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
            tailscale_online: None,
            login_name: None,
            tailnet: None,
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
    fn host_online_observation_distinguishes_disconnected_and_legacy_reports() {
        let mut status = json!({"BackendState":"Running","Self":{"DNSName":"cinema.tail123.ts.net.","TailscaleIPs":["100.80.1.2"]}});
        for (value, expected) in [
            (json!(true), Some(true)),
            (json!(false), Some(false)),
            (json!("true"), None),
            (json!(null), None),
        ] {
            status["Self"]["Online"] = value;
            let address = host_address(&status).expect("valid private identity");
            assert_eq!(address.tailscale_online, expected);
            let mut observation = SetupState::Ready.status();
            observation.tailscale_online = address.tailscale_online;
            let wire = serde_json::to_value(observation).expect("observed status");
            assert_eq!(
                wire["state"], "ready",
                "online is independent from configured forwarding"
            );
            assert_eq!(
                wire.get("tailscale_online")
                    .and_then(serde_json::Value::as_bool),
                expected
            );
        }
        let legacy: HostAddress = serde_json::from_value(
            json!({"ipv4":"100.80.1.2","ipv6":null,"ts_fqdn":"cinema.tail123.ts.net"}),
        )
        .expect("older host helper report");
        assert_eq!(legacy.tailscale_online, None);
        assert!(serde_json::to_value(legacy)
            .expect("legacy address")
            .get("tailscale_online")
            .is_none());
    }

    #[test]
    fn visible_peers_exclude_sharee_nodes_and_unknown_hosts() {
        let status = json!({"BackendState":"Running","Peer":{
            "a":{"DNSName":"source.tail111.ts.net.","TailscaleIPs":["100.64.1.1","fd7a:115c:a1e0::1"],"Online":true},
            "b":{"DNSName":"","TailscaleIPs":["100.64.2.2"],"Online":true,"ShareeNode":true}
        }});
        let peers = parse_visible_peers(&status).expect("running host");
        let source: IpAddr = "100.64.1.1".parse().expect("fixture");
        assert_eq!(peers[&source].dns_name, "source.tail111.ts.net");
        assert_eq!(peers[&source].online, Some(true));
        assert!(peers.contains_key(&"fd7a:115c:a1e0::1".parse::<IpAddr>().expect("fixture")));
        assert!(
            !peers[&"100.64.2.2".parse::<IpAddr>().expect("fixture")].dialable,
            "a sharee node may connect in but cannot be dialled"
        );
        assert!(peers[&source].dialable);
        assert!(
            parse_visible_peers(&json!({"BackendState":"Running","Peer":null}))
                .expect("no peers")
                .is_empty()
        );
        assert!(parse_visible_peers(&json!({"BackendState":"NeedsLogin"})).is_none());
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
