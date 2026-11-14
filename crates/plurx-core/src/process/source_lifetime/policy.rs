//! Immutable node roots and revocable, identity-bound daemon capabilities.
//! Failures name the check, never a path, URL or bearer capability.
use super::arguments::{ArgumentProfile, ArgumentRole};
use super::descriptors::{DaemonFileRegistration, DescriptorManifest, StdioMode};
use super::SourceAccess;
use std::collections::BTreeMap;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

const MAX_CAPABILITIES: usize = 4096;
#[derive(Clone, Debug, PartialEq, Eq)]
struct Roots {
    owned: Vec<PathBuf>,
    tools: Vec<PathBuf>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Identity {
    device: u64,
    inode: u64,
}
#[cfg(unix)]
fn identity(file: &File) -> io::Result<Identity> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata()?;
    Ok(Identity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}
#[cfg(not(unix))]
fn identity(_file: &File) -> io::Result<Identity> {
    Err(io::Error::other("reader descriptor identity requires Unix"))
}
struct Asset {
    path: PathBuf,
    file: Arc<File>,
    identity: Identity,
}
#[derive(Default)]
struct State {
    roots: Option<Roots>,
    next_id: u64,
    brokers: BTreeMap<u64, String>,
    assets: BTreeMap<u64, Asset>,
}
static STATE: LazyLock<Mutex<State>> = LazyLock::new(Default::default);

fn canonical_roots(paths: &[PathBuf]) -> io::Result<Vec<PathBuf>> {
    let mut result = Vec::with_capacity(paths.len());
    for path in paths {
        let path = path.canonicalize()?;
        if !path.is_dir() || path.parent().is_none() {
            return Err(io::Error::other(
                "reader roots must be qualified directories",
            ));
        }
        result.push(path);
    }
    result.sort();
    result.dedup();
    if result.is_empty() {
        return Err(io::Error::other("reader roots cannot be empty"));
    }
    Ok(result)
}

/// Configuration is immutable once installed. Absence grants no verification;
/// callers may avoid asset registration only in that conservative mode.
pub fn daemon_roots_configured() -> io::Result<bool> {
    STATE
        .lock()
        .map(|state| state.roots.is_some())
        .map_err(|_| io::Error::other("reader policy registry poisoned"))
}

/// Configure once during real daemon startup, not for each AppState. Tests
/// without a configured process registry remain conservative and unverified.
pub fn set_daemon_roots(owned: &[PathBuf], tools: &[PathBuf]) -> io::Result<()> {
    let roots = Roots {
        owned: canonical_roots(owned)?,
        tools: canonical_roots(tools)?,
    };
    let mut state = STATE
        .lock()
        .map_err(|_| io::Error::other("reader policy registry poisoned"))?;
    match &state.roots {
        Some(existing) if existing == &roots => Ok(()),
        Some(_) => Err(io::Error::other(
            "reader roots already configured differently",
        )),
        None => {
            state.roots = Some(roots);
            Ok(())
        }
    }
}
/// Prepare only an audited no-input candidate. Unqualified tools retain the
/// caller's program resolution and working directory; setup grants no proof.
pub fn configure_no_input_probe(command: &mut std::process::Command) {
    if let Ok(state) = STATE.lock() {
        configure_no_input_probe_in(&state, command);
    }
}
fn configure_no_input_probe_in(state: &State, command: &mut std::process::Command) {
    let Some(roots) = state.roots.as_ref() else {
        return;
    };
    let program = Path::new(command.get_program());
    if !program.is_absolute() {
        return;
    }
    let Ok(program) = program.canonicalize() else {
        return;
    };
    if !contained(&program, &roots.tools) {
        return;
    }
    let Some(args) = command
        .get_args()
        .map(|arg| arg.to_str())
        .collect::<Option<Vec<_>>>()
    else {
        return;
    };
    if no_input_profile(&program, &args) {
        if let Some(cwd) = roots.owned.first() {
            command.current_dir(cwd);
        }
    }
}
fn contained(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| path.starts_with(root))
}
fn next_id(state: &mut State) -> io::Result<u64> {
    state.next_id = state
        .next_id
        .checked_add(1)
        .ok_or_else(|| io::Error::other("reader capability identity exhausted"))?;
    Ok(state.next_id)
}

/// A guard owns one registration, so dropping an older guard for the same
/// URL cannot revoke a newer/live registration.
pub struct BrokerRegistration {
    id: u64,
}
impl Drop for BrokerRegistration {
    fn drop(&mut self) {
        STATE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .brokers
            .remove(&self.id);
    }
}
fn url(value: &str) -> Result<reqwest::Url, &'static str> {
    let url = reqwest::Url::parse(value).map_err(|_| "invalid_url")?;
    if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return Err("url_credentials_or_fragment");
    }
    Ok(url)
}
fn loopback(url: &reqwest::Url) -> bool {
    url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    })
}
/// Read-only exact-capability observation for lifetime wiring tests and
/// diagnostics. It never registers, extends or authorizes a capability.
pub fn is_registered_broker(value: &str) -> bool {
    let Ok(url) = url(value) else {
        return false;
    };
    STATE.lock().is_ok_and(|state| {
        state
            .brokers
            .values()
            .any(|registered| registered == url.as_str())
    })
}
pub fn register_broker(value: &str) -> io::Result<BrokerRegistration> {
    if value.len() > 4096 {
        return Err(io::Error::other("broker capability exceeds its bound"));
    }
    let url = url(value).map_err(io::Error::other)?;
    if !matches!(url.scheme(), "http" | "https") || !loopback(&url) {
        return Err(io::Error::other(
            "broker capability must be an exact loopback HTTP URL",
        ));
    }
    let mut state = STATE
        .lock()
        .map_err(|_| io::Error::other("reader policy registry poisoned"))?;
    if state.brokers.len() >= MAX_CAPABILITIES {
        return Err(io::Error::other("broker capability registry full"));
    }
    let id = next_id(&mut state)?;
    state.brokers.insert(id, url.to_string());
    Ok(BrokerRegistration { id })
}

pub(super) fn register_asset(file: &File, path: &Path) -> io::Result<DaemonFileRegistration> {
    let canonical = path.canonicalize()?;
    let held = Arc::new(file.try_clone()?);
    let actual = identity(&held)?;
    let named = File::open(&canonical)?;
    if identity(&named)? != actual {
        return Err(io::Error::other("daemon asset and held descriptor differ"));
    }
    let mut state = STATE
        .lock()
        .map_err(|_| io::Error::other("reader policy registry poisoned"))?;
    let roots = state
        .roots
        .as_ref()
        .ok_or_else(|| io::Error::other("reader roots are not configured"))?;
    if !contained(&canonical, &roots.owned) {
        return Err(io::Error::other("daemon asset is outside qualified roots"));
    }
    if state.assets.len() >= MAX_CAPABILITIES {
        return Err(io::Error::other("daemon asset registry full"));
    }
    let id = next_id(&mut state)?;
    state.assets.insert(
        id,
        Asset {
            path: canonical,
            file: held,
            identity: actual,
        },
    );
    Ok(DaemonFileRegistration { id })
}
pub(super) fn unregister_asset(id: u64) {
    STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .assets
        .remove(&id);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Refusal {
    pub(super) check: &'static str,
    pub(super) enforce: bool,
}
impl Refusal {
    fn configured(check: &'static str) -> Self {
        Self {
            check,
            enforce: true,
        }
    }
    fn unavailable(check: &'static str) -> Self {
        Self {
            check,
            enforce: false,
        }
    }
}

fn asset_matches(state: &State, file: &File) -> bool {
    let Ok(held) = identity(file) else {
        return false;
    };
    state.assets.values().any(|asset| {
        asset.identity == held
            && identity(&asset.file).is_ok_and(|registered| registered == held)
            && File::open(&asset.path)
                .and_then(|named| identity(&named))
                .is_ok_and(|named| named == held)
    })
}
fn registered_path(state: &State, path: &Path) -> bool {
    let Ok(canonical) = path.canonicalize() else {
        return false;
    };
    File::open(&canonical).is_ok_and(|file| {
        state.assets.values().any(|asset| asset.path == canonical) && asset_matches(state, &file)
    })
}

#[cfg(unix)]
fn descriptor_safe(state: &State, file: &File) -> bool {
    use std::os::unix::fs::FileTypeExt;
    file.metadata()
        .is_ok_and(|metadata| metadata.file_type().is_fifo() || metadata.file_type().is_socket())
        || asset_matches(state, file)
}

fn manifest_safe(state: &State, manifest: &DescriptorManifest) -> Result<(), &'static str> {
    manifest
        .argument_profile
        .validate_descriptors(manifest.inherited(), manifest.cwd_target)
        .map_err(|_| "drm_device_identity")?;
    let _ = state; // Descriptor inspection is Unix-only; opaque stdio still refuses elsewhere.
    #[cfg(unix)]
    if manifest.inherited().iter().any(|entry| {
        manifest.argument_profile.drm_device() != Some(entry.target())
            && !descriptor_safe(state, entry.file())
    }) {
        return Err("source_descriptor");
    }
    for (_number, mode) in [
        (0, manifest.stdio.stdin),
        (1, manifest.stdio.stdout),
        (2, manifest.stdio.stderr),
    ] {
        match mode {
            StdioMode::Null | StdioMode::Piped => {}
            #[cfg(unix)]
            StdioMode::Descriptor(target)
                if target == _number
                    && manifest
                        .inherited()
                        .iter()
                        .any(|entry| entry.target() == target) => {}
            _ => return Err("unregistered_stdio"),
        }
    }
    #[cfg(unix)]
    if let Some(target) = manifest.cwd_target {
        let entry = manifest
            .inherited()
            .iter()
            .find(|entry| entry.target() == target)
            .ok_or("unregistered_cwd_descriptor")?;
        if !entry
            .file()
            .metadata()
            .is_ok_and(|metadata| metadata.is_dir())
            || !asset_matches(state, entry.file())
        {
            return Err("unregistered_cwd_descriptor");
        }
    }
    Ok(())
}

fn command_program(
    command: &std::process::Command,
    roots: &Roots,
    cwd: &Path,
) -> Result<PathBuf, &'static str> {
    let program = Path::new(command.get_program());
    if program.components().count() > 1 {
        return (if program.is_absolute() {
            program.to_owned()
        } else {
            cwd.join(program)
        })
        .canonicalize()
        .map_err(|_| "program_identity");
    }
    let explicit_path = command.get_envs().find(|(key, _)| *key == "PATH");
    let path = match explicit_path {
        Some((_, Some(path))) => path.to_owned(),
        Some((_, None)) => return Err("program_path_unavailable"),
        None => std::env::var_os("PATH").ok_or("program_path_unavailable")?,
    };
    // Search in the real order, not only trusted directories: a shadowing
    // executable must be refused rather than silently resolving another file.
    for root in std::env::split_paths(&path) {
        let candidate = (if root.is_absolute() {
            root
        } else {
            cwd.join(root)
        })
        .join(program);
        if candidate.is_file() {
            let canonical = candidate.canonicalize().map_err(|_| "program_identity")?;
            if !contained(&canonical, &roots.tools) {
                return Err("program_outside_tool_roots");
            }
            return Ok(canonical);
        }
    }
    Err("program_identity")
}

fn canonical_argument(path: &Path, cwd: &Path) -> Result<PathBuf, &'static str> {
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        cwd.join(path)
    };
    if let Ok(canonical) = path.canonicalize() {
        return Ok(canonical);
    }
    // Output patterns/files need not exist yet. Only one nonexistent final
    // component is allowed; unresolved parent chains fail closed.
    let parent = path
        .parent()
        .ok_or("path_identity")?
        .canonicalize()
        .map_err(|_| "path_identity")?;
    let name = path.file_name().ok_or("path_identity")?;
    Ok(parent.join(name))
}
fn fd_token(value: &str) -> Option<Result<i32, &'static str>> {
    for prefix in ["/proc/self/fd/", "/dev/fd/", "pipe:"] {
        if let Some(number) = value.strip_prefix(prefix) {
            return Some(
                number
                    .parse::<i32>()
                    .map_err(|_| "invalid_descriptor_token"),
            );
        }
    }
    None
}
fn registered_descriptor(value: i32, manifest: &DescriptorManifest) -> bool {
    if manifest.argument_profile.drm_device() == Some(value) {
        return false;
    }
    if matches!(value, 1 | 2) {
        let mode = if value == 1 {
            manifest.stdio.stdout
        } else {
            manifest.stdio.stderr
        };
        if matches!(mode, StdioMode::Null | StdioMode::Piped) {
            return true;
        }
    }
    if value == 0 && matches!(manifest.stdio.stdin, StdioMode::Null | StdioMode::Piped) {
        return true;
    }
    #[cfg(unix)]
    {
        manifest
            .inherited()
            .iter()
            .any(|entry| entry.target() == value)
    }
    #[cfg(not(unix))]
    {
        false
    }
}
fn tuner_allowed(access: SourceAccess, purpose: &str, schemes: &[&str], scheme: &str) -> bool {
    access == SourceAccess::NoSource
        && matches!(
            purpose,
            "Live TV stream"
                | "Live TV source probe"
                | "Live TV readiness probe"
                | "Live TV caption probe"
        )
        && matches!(scheme, "http" | "https" | "rtsp" | "udp" | "rtp")
        && schemes.contains(&scheme)
}

#[derive(Clone, Copy)]
struct InputContext<'a> {
    state: &'a State,
    roots: &'a Roots,
    manifest: &'a DescriptorManifest,
    access: SourceAccess,
    purpose: &'a str,
    tuner_schemes: &'a [&'a str],
}

fn check_fragment(value: &str, cwd: &Path, context: InputContext<'_>) -> Result<(), &'static str> {
    let InputContext {
        state,
        roots,
        manifest,
        access,
        purpose,
        tuner_schemes,
    } = context;
    if value.is_empty() {
        return Ok(());
    }
    if let Some(target) = fd_token(value) {
        if !registered_descriptor(target?, manifest) {
            return Err("unregistered_descriptor_token");
        }
        return Ok(());
    }
    if value.contains("://") {
        let parsed = url(value)?;
        if loopback(&parsed) {
            if !matches!(parsed.scheme(), "http" | "https")
                || !state
                    .brokers
                    .values()
                    .any(|registered| registered == parsed.as_str())
            {
                return Err("unregistered_broker_url");
            }
        } else if !tuner_allowed(access, purpose, tuner_schemes, parsed.scheme()) {
            return Err("non_broker_url");
        }
        return Ok(());
    }
    // file:/path and filter option separators must not disguise local inputs.
    let value = value.strip_prefix("file:").unwrap_or(value);
    for path_value in value.split(':') {
        let candidate = if Path::new(path_value).is_absolute() {
            PathBuf::from(path_value)
        } else {
            cwd.join(path_value)
        };
        let path_like = path_value.contains('/')
            || Path::new(path_value).is_absolute()
            || path_value.starts_with("./")
            || path_value.starts_with("../")
            || candidate.is_file();
        if !path_like {
            continue;
        }
        let canonical = canonical_argument(Path::new(path_value), cwd)?;
        if !contained(&canonical, &roots.owned) && !contained(&canonical, &roots.tools) {
            return Err("path_outside_daemon_roots");
        }
        // A library may itself be configured below the data directory. Merely
        // being on the daemon's device/root cannot bless a regular input.
        if canonical.is_file() && !registered_path(state, &canonical) {
            return Err("unregistered_daemon_file");
        }
    }
    Ok(())
}

static URL_FRAGMENTS: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r#"[A-Za-z][A-Za-z0-9+.-]*://[^\s\"'|,\[\];]+"#)
        .expect("fixed URL token grammar")
});

static FILTER_INPUTS: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r#"(?:movie|amovie|subtitles|ass|filename|textfile|fontfile)\s*=\s*([^:;,]+)"#,
    )
    .expect("fixed filter input grammar")
});

fn check_argument(arg: &str, cwd: &Path, context: InputContext<'_>) -> Result<(), &'static str> {
    let InputContext {
        state, manifest, ..
    } = context;
    let unescaped = arg.replace("\\:", ":").replace("\\,", ",");
    // Preserve full direct URLs (including query '=' and encoded delimiters).
    if unescaped.split_once("://").is_some_and(|(scheme, _)| {
        !scheme.is_empty()
            && scheme
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"+.-".contains(&byte))
    }) {
        return check_fragment(&unescaped, cwd, context);
    }
    for capture in FILTER_INPUTS.captures_iter(&unescaped) {
        let value = capture
            .get(1)
            .ok_or("filter_input")?
            .as_str()
            .trim()
            .trim_matches(['\'', '"']);
        let value = value.strip_prefix("filename=").unwrap_or(value);
        if let Some(target) = fd_token(value) {
            if !registered_descriptor(target?, manifest) {
                return Err("unregistered_descriptor_token");
            }
            continue;
        }
        let path = if Path::new(value).is_absolute() {
            PathBuf::from(value)
        } else {
            cwd.join(value)
        };
        if !registered_path(state, &path) {
            return Err("unregistered_filter_input");
        }
    }
    let mut remaining = unescaped.clone();
    let found = URL_FRAGMENTS.find_iter(&unescaped).collect::<Vec<_>>();
    for matched in &found {
        check_fragment(matched.as_str(), cwd, context)?;
    }
    for matched in found.into_iter().rev() {
        remaining.replace_range(matched.range(), " ");
    }
    for fragment in remaining.split(['=', '\'', '"', ',', ';', '[', ']', '|', ' ', '\t', '\n']) {
        if [
            "data:", "concat:", "concatf:", "subfile:", "crypto:", "cache:", "async:", "bluray:",
            "dvd:",
        ]
        .iter()
        .any(|prefix| fragment.starts_with(prefix))
        {
            return Err("unsupported_input_protocol");
        }
        check_fragment(fragment, cwd, context)?;
    }
    Ok(())
}

fn check_script(
    path: &Path,
    cwd: &Path,
    context: InputContext<'_>,
    concat: bool,
) -> Result<(), &'static str> {
    let InputContext { state, .. } = context;
    use std::io::Read;
    if !registered_path(state, path) {
        return Err("unregistered_script_input");
    }
    let mut text = String::new();
    File::open(path)
        .map_err(|_| "script_identity")?
        .take(65_537)
        .read_to_string(&mut text)
        .map_err(|_| "script_content")?;
    if text.len() > 65_536 {
        return Err("script_content_bound");
    }
    if text.contains("://") {
        return Err("script_network_input_unqualified");
    }
    let base = path.parent().ok_or("script_identity")?;
    let reference = |value: &str| -> Result<(), &'static str> {
        let value = value.trim().trim_matches(['\'', '"']);
        let target = if Path::new(value).is_absolute() {
            PathBuf::from(value)
        } else {
            base.join(value)
        };
        if !target.is_file() || !registered_path(state, &target) {
            return Err("unregistered_script_reference");
        }
        Ok(())
    };
    if concat
        || text
            .lines()
            .next()
            .is_some_and(|line| line.trim() == "ffconcat version 1.0")
    {
        for line in text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
        {
            if line == "ffconcat version 1.0" {
                continue;
            }
            if let Some(value) = line.strip_prefix("file ") {
                reference(value)?;
                continue;
            }
            if ["duration ", "inpoint ", "outpoint "].iter().any(|prefix| {
                line.strip_prefix(prefix)
                    .is_some_and(|value| value.parse::<f64>().is_ok_and(f64::is_finite))
            }) {
                continue;
            }
            // No arbitrary concat options or future directives acquire trust
            // merely because the daemon registered the script's own inode.
            return Err("unsupported_script_directive");
        }
    } else if text
        .lines()
        .next()
        .is_some_and(|line| line.trim() == "#EXTM3U")
    {
        for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
            if !line.starts_with('#') {
                reference(line)?;
                continue;
            }
            if let Some((_, value)) = line.split_once("URI=") {
                let value = if let Some(quoted) = value.strip_prefix('"') {
                    quoted.split('"').next().ok_or("script_content")?
                } else {
                    value.split(',').next().ok_or("script_content")?
                };
                reference(value)?;
            }
        }
    } else {
        // Filter scripts use the child's cwd; the general scanner checks all
        // path fragments. Known file-reading filter operands additionally
        // require a registered asset even for extensionless/nonexistent names.
        static INPUTS: LazyLock<regex::Regex> = LazyLock::new(|| {
            regex::Regex::new(
                r#"(?:movie|amovie|subtitles|ass|filename|textfile|fontfile)\s*=\s*([^:;,]+)"#,
            )
            .expect("fixed filter input grammar")
        });
        for capture in INPUTS.captures_iter(&text) {
            let value = capture
                .get(1)
                .ok_or("script_content")?
                .as_str()
                .trim()
                .trim_matches(['\'', '"']);
            let target = if Path::new(value).is_absolute() {
                PathBuf::from(value)
            } else {
                cwd.join(value)
            };
            if !registered_path(state, &target) {
                return Err("unregistered_script_reference");
            }
        }
    }
    check_argument(&text, base, context)
}

/// Installed tools are trusted operator artifacts. These are the only argv
/// shapes audited to terminate without opening media; tool-root membership
/// alone grants no authority to an arbitrary executable or option grammar.
fn no_input_profile(program: &Path, args: &[&str]) -> bool {
    if program.file_name().is_some_and(|value| value == "true") {
        return args.is_empty();
    }
    let args = args.strip_prefix(&["-hide_banner"]).unwrap_or(args);
    match program.file_name().and_then(|value| value.to_str()) {
        Some("ffprobe") => args == ["-version"],
        Some("ffmpeg") => match args {
            [option] => [
                "-version",
                "-filters",
                "-encoders",
                "-decoders",
                "-hwaccels",
                "-bsfs",
            ]
            .contains(option),
            ["-h", topic] => topic.strip_prefix("filter=").is_some_and(|name| {
                !name.is_empty()
                    && name
                        .bytes()
                        .all(|value| value.is_ascii_alphanumeric() || value == b'_')
            }),
            _ => false,
        },
        _ => false,
    }
}

fn verify_state(
    state: &State,
    command: &std::process::Command,
    access: SourceAccess,
    manifest: Option<&DescriptorManifest>,
    purpose: &str,
    tuner_schemes: &[&str],
) -> Result<(), Refusal> {
    if access == SourceAccess::Unknown {
        return Err(Refusal::unavailable("undeclared"));
    }
    let roots = state
        .roots
        .as_ref()
        .ok_or_else(|| Refusal::unavailable("roots_unconfigured"))?;
    let manifest = manifest.ok_or_else(|| Refusal::configured("missing_descriptor_manifest"))?;
    if !manifest.matches(command) {
        return Err(Refusal::configured("descriptor_manifest_command_mismatch"));
    }
    if super::has_descriptor_handoff(command) {
        return Err(Refusal::configured("legacy_descriptor_handoff"));
    }
    manifest_safe(state, manifest).map_err(Refusal::configured)?;
    let context = InputContext {
        state,
        roots,
        manifest,
        access,
        purpose,
        tuner_schemes,
    };
    let cwd = if let Some(target) = manifest.cwd_target {
        #[cfg(unix)]
        {
            let entry = manifest
                .inherited()
                .iter()
                .find(|entry| entry.target() == target)
                .ok_or_else(|| Refusal::configured("unregistered_cwd_descriptor"))?;
            state
                .assets
                .values()
                .find(|asset| identity(entry.file()).is_ok_and(|held| held == asset.identity))
                .map(|asset| asset.path.clone())
                .ok_or_else(|| Refusal::configured("unregistered_cwd_descriptor"))?
        }
        #[cfg(not(unix))]
        {
            let _ = target;
            return Err(Refusal::unavailable("unsupported_descriptor_platform"));
        }
    } else {
        command
            .get_current_dir()
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
            .canonicalize()
            .map_err(|_| Refusal::configured("cwd_identity"))?
    };
    let program = command_program(command, roots, &cwd).map_err(Refusal::configured)?;
    if !contained(&program, &roots.tools) {
        return Err(Refusal::configured("program_outside_tool_roots"));
    }
    let args = command
        .get_args()
        .map(|arg| {
            arg.to_str()
                .ok_or_else(|| Refusal::configured("non_utf8_argument"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    // The final launcher replaces the environment of an approved probe. An
    // absolute executable keeps that isolation from changing PATH resolution.
    // Relative/custom launches still run, without retaining recovery proof.
    if !Path::new(command.get_program()).is_absolute() {
        return Err(Refusal::unavailable("probe_program_not_absolute"));
    }
    if access == SourceAccess::MetadataOnly {
        if manifest.argument_profile != ArgumentProfile::Default {
            return Err(Refusal::unavailable(
                "argument_profile_confinement_unqualified",
            ));
        }
        if program.file_name().is_none_or(|name| name != "find")
            || args.len() != 8
            || args[0] != "-H"
            || !args[1].ends_with("/.")
            || args[2..] != ["-mindepth", "1", "-maxdepth", "1", "-print", "-quit"]
            || !manifest.inherited().is_empty()
            || manifest.stdio.stdin != StdioMode::Null
            || !matches!(manifest.stdio.stdout, StdioMode::Null | StdioMode::Piped)
            || !matches!(manifest.stdio.stderr, StdioMode::Null | StdioMode::Piped)
        {
            return Err(Refusal::configured("metadata_find_shape"));
        }
        return Ok(());
    }
    if !contained(&cwd, &roots.owned) {
        return Err(Refusal::configured("cwd_outside_daemon_roots"));
    }
    let roles = manifest
        .argument_profile
        .argument_roles(&args)
        .map_err(Refusal::configured)?;
    let probe = program.file_name().is_some_and(|name| name == "ffprobe");
    let mut media_input = manifest.stdio.stdin != StdioMode::Null;
    let mut source_next = false;
    let mut script_next = false;
    let mut format_next = false;
    let mut concat = false;
    let mut lavfi = false;
    for (index, arg) in args.iter().enumerate() {
        let role = roles.get(index).copied().unwrap_or(ArgumentRole::Ordinary);
        if format_next {
            concat = *arg == "concat";
            lavfi = *arg == "lavfi";
            format_next = false;
        }
        let input_path = if Path::new(arg).is_absolute() {
            PathBuf::from(arg)
        } else {
            cwd.join(arg)
        };
        let input =
            source_next || script_next || (probe && !arg.starts_with('-') && input_path.exists());
        if input {
            if role != ArgumentRole::Ordinary {
                return Err(Refusal::configured("argument_profile_input_role"));
            }
            media_input = true;
            let list = script_next
                || concat
                || matches!(
                    Path::new(arg).extension().and_then(|value| value.to_str()),
                    Some("ffconcat" | "m3u8" | "txt")
                );
            if list && arg.contains("://") {
                check_argument(arg, &cwd, context).map_err(Refusal::configured)?;
                return Err(Refusal::unavailable("broker_confinement_unqualified"));
            }
            if list {
                check_script(&input_path, &cwd, context, concat).map_err(Refusal::configured)?;
            } else if !arg.contains("://")
                && fd_token(arg).is_none()
                && !(lavfi && source_next)
                && !registered_path(state, &input_path)
            {
                return Err(Refusal::configured("unregistered_file_input"));
            }
            source_next = false;
            script_next = false;
        }
        if FILTER_INPUTS.is_match(arg) {
            media_input = true;
        }
        if role == ArgumentRole::Ordinary {
            check_argument(arg, &cwd, context).map_err(Refusal::configured)?;
        }
        if matches!(*arg, "-filter_script" | "-filter_complex_script") {
            script_next = true;
        }
        if *arg == "-i" {
            source_next = true;
        }
        if *arg == "-f" {
            format_next = true;
        }
    }
    if source_next || script_next || format_next {
        return Err(Refusal::configured("missing_option_value"));
    }
    // A live URL proves current ownership, not what a demuxer can open after
    // reading its bytes. Auto-probed lists, external tracks and redirects need
    // an artifact-bound positive input profile; none is qualified yet. This
    // refusal affects recovery proof only and is not a playback enable gate.
    if access == SourceAccess::Brokered {
        return Err(Refusal::unavailable("broker_confinement_unqualified"));
    }
    if args.iter().any(|arg| arg.contains("://")) {
        return Err(Refusal::unavailable(
            "network_input_confinement_unqualified",
        ));
    }
    if media_input {
        return Err(Refusal::unavailable("asset_input_confinement_unqualified"));
    }
    if manifest.argument_profile != ArgumentProfile::Default {
        return Err(Refusal::unavailable(
            "argument_profile_confinement_unqualified",
        ));
    }
    if !no_input_profile(&program, &args) {
        return Err(Refusal::configured("no_input_profile"));
    }
    Ok(())
}

pub(super) fn verify(
    command: &std::process::Command,
    access: SourceAccess,
    manifest: Option<&DescriptorManifest>,
    purpose: &str,
    tuner_schemes: &[&str],
) -> Result<(), Refusal> {
    let state = STATE
        .lock()
        .map_err(|_| Refusal::unavailable("policy_registry_poisoned"))?;
    verify_state(&state, command, access, manifest, purpose, tuner_schemes)
}

#[cfg(all(test, unix))]
mod tests {
    use super::super::descriptors::{InheritedFd, StdioFacts};
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    struct Fixture {
        _root: tempfile::TempDir,
        state: State,
        data: PathBuf,
        media: PathBuf,
        tools: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().expect("fixture");
            let data = root.path().join("data");
            let media = root.path().join("media");
            let tools = root.path().join("tools");
            for path in [&data, &media, &tools] {
                std::fs::create_dir(path).expect("directory");
            }
            for tool in ["ffmpeg", "ffprobe", "find"] {
                let path = tools.join(tool);
                std::fs::write(&path, b"fixture").expect("tool");
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
                    .expect("mode");
            }
            let state = State {
                roots: Some(Roots {
                    owned: vec![data.canonicalize().expect("data")],
                    tools: vec![tools.canonicalize().expect("tools")],
                }),
                ..State::default()
            };
            Self {
                _root: root,
                state,
                data,
                media,
                tools,
            }
        }
        fn command(&self, tool: &str, args: &[&str]) -> std::process::Command {
            let mut command = std::process::Command::new(self.tools.join(tool));
            command.current_dir(&self.data).args(args);
            command
        }
        fn manifest(
            &self,
            command: &std::process::Command,
            entries: Vec<InheritedFd>,
        ) -> DescriptorManifest {
            let manifest =
                DescriptorManifest::new(entries, StdioFacts::CAPTURED, None).expect("manifest");
            manifest.bind(command).expect("binding");
            manifest
        }
        fn verify(
            &self,
            command: &std::process::Command,
            access: SourceAccess,
        ) -> Result<(), Refusal> {
            let manifest = self.manifest(command, vec![]);
            verify_state(
                &self.state,
                command,
                access,
                Some(&manifest),
                "policy fixture",
                &[],
            )
        }
        fn asset(&mut self, path: &Path) {
            let file = Arc::new(File::open(path).expect("asset"));
            let id = identity(&file).expect("identity");
            self.state.assets.insert(
                self.state.assets.len() as u64,
                Asset {
                    path: path.canonicalize().expect("canonical"),
                    file,
                    identity: id,
                },
            );
        }
    }
    #[test]
    fn no_source_launch_with_daemon_paths_and_a_real_pipe_verifies() {
        use std::os::fd::OwnedFd;
        let mut fixture = Fixture::new();
        let asset = fixture.data.join("generated.wav");
        std::fs::write(&asset, b"generated").expect("asset");
        fixture.asset(&asset);
        let (socket, _other) = std::os::unix::net::UnixStream::pair().expect("pipe-like socket");
        let owned: OwnedFd = socket.into();
        let file = File::from(owned);
        let mut command = fixture.command("ffmpeg", &["-version"]);
        command.stdin(std::process::Stdio::null());
        let manifest = fixture.manifest(
            &command,
            vec![
                InheritedFd::duplicate(&file, 6).expect("pipe descriptor"),
                InheritedFd::duplicate(&File::open(&asset).expect("auxiliary"), 7)
                    .expect("registered auxiliary descriptor"),
            ],
        );
        assert_eq!(
            verify_state(
                &fixture.state,
                &command,
                SourceAccess::NoSource,
                Some(&manifest),
                "policy fixture",
                &[]
            ),
            Ok(())
        );
    }
    #[test]
    fn unqualified_probe_setup_preserves_command_resolution_and_cwd() {
        let fixture = Fixture::new();
        let custom = fixture.tools.join("custom-ffmpeg");
        std::fs::write(&custom, b"custom tool").expect("custom tool");
        for (program, args) in [
            (PathBuf::from("bin/ffmpeg"), vec!["-version"]),
            (custom, vec!["-version"]),
            (fixture.tools.join("ffmpeg"), vec!["-version", "movie"]),
            (fixture.media.join("ffmpeg"), vec!["-version"]),
        ] {
            let mut command = std::process::Command::new(&program);
            command.args(args).current_dir(&fixture.media);
            configure_no_input_probe_in(&fixture.state, &mut command);
            assert_eq!(command.get_program(), program.as_os_str());
            assert_eq!(command.get_current_dir(), Some(fixture.media.as_path()));
        }
        let mut qualified = fixture.command("ffmpeg", &["-version"]);
        qualified.current_dir(&fixture.media);
        configure_no_input_probe_in(&fixture.state, &mut qualified);
        assert_eq!(qualified.get_current_dir(), Some(fixture.data.as_path()));
    }

    #[test]
    fn trusted_tool_roots_do_not_approve_computed_reads_or_extra_probe_options() {
        let fixture = Fixture::new();
        let awk = fixture.tools.join("awk");
        std::fs::write(&awk, b"trusted installed non-profile tool").expect("awk fixture");
        let command = fixture.command(
            "awk",
            &[r#"BEGIN { p=sprintf("%c",47) "library"; getline x < p }"#],
        );
        assert_eq!(
            fixture
                .verify(&command, SourceAccess::NoSource)
                .expect_err("arbitrary trusted-root tool must stay unverified")
                .check,
            "no_input_profile"
        );
        for args in [
            vec!["-version", "movie"],
            vec!["-version", "-report"],
            vec!["-h", "filter=movie;amovie"],
            vec!["-filters", "-cpuflags", "all"],
        ] {
            assert!(fixture
                .verify(&fixture.command("ffmpeg", &args), SourceAccess::NoSource)
                .is_err());
        }
        for args in [
            vec!["-hide_banner", "-decoders"],
            vec!["-h", "filter=scale_cuda"],
        ] {
            assert_eq!(
                fixture.verify(&fixture.command("ffmpeg", &args), SourceAccess::NoSource),
                Ok(())
            );
        }
    }

    #[test]
    fn brokered_declaration_with_a_media_path_is_unknown() {
        let fixture = Fixture::new();
        let media = fixture.media.join("movie.mkv");
        std::fs::write(&media, b"source").expect("media");
        let command = fixture.command("ffmpeg", &["-i", media.to_str().expect("path")]);
        assert!(fixture.verify(&command, SourceAccess::Brokered).is_err());
    }
    #[test]
    fn data_root_does_not_register_a_media_file_as_an_asset() {
        let fixture = Fixture::new();
        let media = fixture.data.join("movie.mkv");
        std::fs::write(&media, b"source").expect("media");
        let command = fixture.command("ffmpeg", &["-i", media.to_str().expect("path")]);
        assert!(fixture.verify(&command, SourceAccess::NoSource).is_err());
        let file = File::open(&media).expect("source");
        let command = fixture.command("ffmpeg", &["-i", "/dev/fd/3"]);
        let manifest = fixture.manifest(
            &command,
            vec![InheritedFd::duplicate(&file, 3).expect("source descriptor")],
        );
        assert_eq!(
            verify_state(
                &fixture.state,
                &command,
                SourceAccess::NoSource,
                Some(&manifest),
                "policy fixture",
                &[]
            )
            .expect_err("not an asset")
            .check,
            "source_descriptor"
        );
    }
    #[test]
    fn a_registered_asset_does_not_approve_a_replaced_path_or_regular_stdin() {
        let mut fixture = Fixture::new();
        let path = fixture.data.join("generated.wav");
        std::fs::write(&path, b"generated").expect("asset");
        fixture.asset(&path);
        let held = File::open(&path).expect("held asset");
        let command = fixture.command("ffmpeg", &["-i", path.to_str().expect("path")]);
        let refusal = fixture
            .verify(&command, SourceAccess::NoSource)
            .expect_err("registered media bytes are not a parser confinement profile");
        assert_eq!(refusal.check, "asset_input_confinement_unqualified");
        assert!(!refusal.enforce);
        std::fs::rename(&path, fixture.data.join("retired.wav")).expect("retire asset");
        std::fs::write(&path, b"different source").expect("replacement");
        let manifest = fixture.manifest(
            &command,
            vec![InheritedFd::duplicate(&held, 3).expect("held old inode")],
        );
        assert!(verify_state(
            &fixture.state,
            &command,
            SourceAccess::NoSource,
            Some(&manifest),
            "policy fixture",
            &[]
        )
        .is_err());
        let source = File::open(&path).expect("new raw file");
        let command = fixture.command("ffmpeg", &["-version"]);
        let manifest = DescriptorManifest::new(
            vec![InheritedFd::duplicate(&source, 0).expect("stdin")],
            StdioFacts {
                stdin: StdioMode::Descriptor(0),
                ..StdioFacts::CAPTURED
            },
            None,
        )
        .expect("manifest");
        manifest.bind(&command).expect("bind");
        assert_eq!(
            verify_state(
                &fixture.state,
                &command,
                SourceAccess::NoSource,
                Some(&manifest),
                "policy fixture",
                &[]
            )
            .expect_err("regular stdin is source")
            .check,
            "source_descriptor"
        );
    }
    #[test]
    fn cwd_relative_path_and_symlink_alias_are_unknown() {
        let fixture = Fixture::new();
        let source = fixture.media.join("movie.mkv");
        std::fs::write(&source, b"source").expect("source");
        let mut command = fixture.command("ffmpeg", &["-i", "movie.mkv"]);
        command.current_dir(&fixture.media);
        assert!(fixture.verify(&command, SourceAccess::NoSource).is_err());
        let alias = fixture.data.join("alias.mkv");
        std::os::unix::fs::symlink(&source, &alias).expect("alias");
        let command = fixture.command("ffmpeg", &["-i", alias.to_str().expect("alias path")]);
        assert!(fixture.verify(&command, SourceAccess::NoSource).is_err());
    }
    #[test]
    fn directories_unregistered_lists_and_embedded_media_paths_are_unknown() {
        let fixture = Fixture::new();
        let list = fixture.data.join("list.ffconcat");
        std::fs::write(&list, "ffconcat version 1.0\nfile /media/movie.mkv\n").expect("list");
        for args in [
            vec!["-i", fixture.media.to_str().expect("path")],
            vec!["-f", "concat", "-i", list.to_str().expect("path")],
            vec!["-vf", "subtitles=filename=/media/movie.vtt"],
            vec!["-vf", "movie=/media/movie.mkv"],
        ] {
            assert!(fixture
                .verify(&fixture.command("ffmpeg", &args), SourceAccess::NoSource)
                .is_err());
        }
    }
    #[test]
    fn unregistered_stdio_and_a_different_or_mutated_command_are_unknown() {
        let fixture = Fixture::new();
        let mut command = fixture.command("ffmpeg", &["-version"]);
        let manifest = fixture.manifest(&command, vec![]);
        command.arg("-i").arg("movie.mkv");
        assert_eq!(
            verify_state(
                &fixture.state,
                &command,
                SourceAccess::NoSource,
                Some(&manifest),
                "policy fixture",
                &[]
            )
            .expect_err("command changed")
            .check,
            "descriptor_manifest_command_mismatch"
        );
        let other = fixture.command("ffmpeg", &["-version"]);
        assert!(verify_state(
            &fixture.state,
            &other,
            SourceAccess::NoSource,
            Some(&manifest),
            "policy fixture",
            &[]
        )
        .is_err());
        let manifest = DescriptorManifest::new(
            vec![],
            StdioFacts {
                stdin: StdioMode::Inherited,
                ..StdioFacts::CAPTURED
            },
            None,
        )
        .expect("manifest");
        manifest.bind(&other).expect("binding");
        assert_eq!(
            verify_state(
                &fixture.state,
                &other,
                SourceAccess::NoSource,
                Some(&manifest),
                "policy fixture",
                &[]
            )
            .expect_err("opaque stdin")
            .check,
            "unregistered_stdio"
        );
    }
    #[test]
    fn legacy_descriptor_tag_cannot_be_whitewashed_by_an_empty_manifest() {
        let fixture = Fixture::new();
        let mut command = fixture.command("ffmpeg", &["-version"]);
        command.env(super::super::DESCRIPTOR_HANDOFF, "1");
        assert_eq!(
            fixture
                .verify(&command, SourceAccess::NoSource)
                .expect_err("tagged extra hook")
                .check,
            "legacy_descriptor_handoff"
        );
    }
    #[test]
    fn exact_metadata_find_shape_verifies_without_reading_a_source() {
        let fixture = Fixture::new();
        let root = fixture.media.join(".");
        let mut command = fixture.command(
            "find",
            &[
                "-H",
                root.to_str().expect("path"),
                "-mindepth",
                "1",
                "-maxdepth",
                "1",
                "-print",
                "-quit",
            ],
        );
        command.current_dir(&fixture.media);
        let manifest = fixture.manifest(&command, vec![]);
        assert_eq!(
            verify_state(
                &fixture.state,
                &command,
                SourceAccess::MetadataOnly,
                Some(&manifest),
                "library root reachability probe",
                &[]
            ),
            Ok(())
        );
        command.arg("-delete");
        let manifest = fixture.manifest(&command, vec![]);
        assert!(verify_state(
            &fixture.state,
            &command,
            SourceAccess::MetadataOnly,
            Some(&manifest),
            "library root reachability probe",
            &[]
        )
        .is_err());
    }
    #[test]
    fn live_broker_url_alone_does_not_prove_content_confinement_and_dead_url_is_refused() {
        let mut fixture = Fixture::new();
        let value = "http://127.0.0.1:12345/nonce/media?x=1&y=2";
        fixture.state.brokers.insert(1, value.into());
        for value in [value, "http://127.0.0.1:12345/nonce/playlist.m3u8"] {
            if value.ends_with("m3u8") {
                fixture.state.brokers.insert(2, value.into());
            }
            let command = fixture.command("ffmpeg", &["-i", value]);
            let refusal = fixture
                .verify(&command, SourceAccess::Brokered)
                .expect_err("URL is not confinement");
            assert_eq!(refusal.check, "broker_confinement_unqualified");
            assert!(
                !refusal.enforce,
                "qualification absence is not a playback gate"
            );
        }
        fixture.state.brokers.clear();
        let command = fixture.command("ffmpeg", &["-i", value]);
        assert_eq!(
            fixture
                .verify(&command, SourceAccess::Brokered)
                .expect_err("dead capability")
                .check,
            "unregistered_broker_url"
        );
    }
    #[test]
    fn absent_roots_are_conservative_without_a_debug_assertion() {
        let fixture = Fixture::new();
        let command = fixture.command("ffmpeg", &["-version"]);
        let manifest = fixture.manifest(&command, vec![]);
        let refusal = verify_state(
            &State::default(),
            &command,
            SourceAccess::NoSource,
            Some(&manifest),
            "policy fixture",
            &[],
        )
        .expect_err("not initialized");
        assert_eq!(refusal.check, "roots_unconfigured");
        assert!(!refusal.enforce);
    }
    #[test]
    fn broker_registration_guards_revoke_only_their_own_exact_capability() {
        let value = format!(
            "http://127.0.0.1:12345/{}/media?x=1&y=2",
            uuid::Uuid::new_v4()
        );
        let first = register_broker(&value).expect("first");
        let second = register_broker(&value).expect("second");
        assert!(is_registered_broker(&value));
        assert!(!is_registered_broker(&format!("{value}&z=3")));
        drop(first);
        assert!(is_registered_broker(&value));
        drop(second);
        assert!(!is_registered_broker(&value));
    }
    #[test]
    fn typed_rates_preserve_capability_checks_and_never_approve_confinement() {
        let mut fixture = Fixture::new();
        let broker = "http://127.0.0.1:12345/source";
        fixture.state.brokers.insert(1, broker.into());
        let check = |fixture: &Fixture, command: &std::process::Command, access| {
            let manifest = DescriptorManifest::new_with_profile(
                vec![],
                StdioFacts::CAPTURED,
                None,
                ArgumentProfile::Ffmpeg { drm_device: None },
            )
            .expect("typed manifest");
            manifest.bind(command).expect("final binding");
            verify_state(
                &fixture.state,
                command,
                access,
                Some(&manifest),
                "typed protocol fixture",
                &[],
            )
            .expect_err("syntax profile is not confinement proof")
        };
        let command = fixture.command(
            "ffmpeg",
            &["-hide_banner", "-r", "24000/1001", "-i", broker],
        );
        let refusal = check(&fixture, &command, SourceAccess::Brokered);
        assert_eq!(refusal.check, "broker_confinement_unqualified");
        assert!(!refusal.enforce);
        fixture.state.brokers.clear();
        assert_eq!(
            check(&fixture, &command, SourceAccess::Brokered).check,
            "unregistered_broker_url"
        );
        let input = fixture.command("ffmpeg", &["-r", "24000/1001", "-i", "0/1"]);
        assert_eq!(
            check(&fixture, &input, SourceAccess::Brokered).check,
            "unregistered_file_input"
        );
        let probe = fixture.command("ffmpeg", &["-version"]);
        let refusal = check(&fixture, &probe, SourceAccess::NoSource);
        assert_eq!(refusal.check, "argument_profile_confinement_unqualified");
        assert!(!refusal.enforce);
    }

    #[test]
    fn typed_renderer_checks_bound_descriptors_brokers_and_command_mutation() {
        let mut fixture = Fixture::new();
        let data = fixture.data.clone();
        fixture.asset(&data);
        let directory = File::open(&data).expect("directory");
        let (socket, _other) = std::os::unix::net::UnixStream::pair().expect("pipe-like output");
        let output = File::from(std::os::fd::OwnedFd::from(socket));
        let broker = "http://127.0.0.1:12345/source";
        fixture.state.brokers.insert(1, broker.into());
        let args = [
            broker,
            "/proc/self/fd/4",
            "0",
            "3",
            "1920",
            "1080",
            "0",
            "0",
            "0",
            "bt2020-pq-master-clip",
            "/proc/self/fd/5",
            "0/1",
            "1/1",
            "512",
            "fel",
            "1920",
            "1080",
            "pq-bilinear-rgb48-v1",
            "--held-source-origin",
            "0/1",
        ];
        let mut command = fixture.command("ffmpeg", &args);
        let manifest = DescriptorManifest::new_with_profile(
            vec![
                InheritedFd::duplicate(&directory, 4).expect("directory pin"),
                InheritedFd::duplicate(&output, 5).expect("output pin"),
            ],
            StdioFacts::CAPTURED,
            None,
            ArgumentProfile::FelRendererWindowV1,
        )
        .expect("typed manifest");
        manifest.bind(&command).expect("final binding");
        let refusal = verify_state(
            &fixture.state,
            &command,
            SourceAccess::Brokered,
            Some(&manifest),
            "typed renderer fixture",
            &[],
        )
        .expect_err("no confinement approval");
        assert_eq!(refusal.check, "broker_confinement_unqualified");
        assert!(!refusal.enforce);
        command.env("LD_PRELOAD", "unregistered.so");
        assert_eq!(
            verify_state(
                &fixture.state,
                &command,
                SourceAccess::Brokered,
                Some(&manifest),
                "typed renderer fixture",
                &[]
            )
            .expect_err("command changed")
            .check,
            "descriptor_manifest_command_mismatch"
        );
    }
}
