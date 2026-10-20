//! Exact-byte signed release manifests, bounded public transport and package cohorts.
use crate::{
    installation::{Architecture, InstallMethod, InstallationV1, REPOSITORY},
    protocol::ErrorCode,
    secure_fs::SecureDir,
    Error, Result,
};
use ring::signature::{UnparsedPublicKey, ED25519};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Read, Write},
    path::Path,
    time::Duration,
};

pub const MAX_MANIFEST: usize = 1024 * 1024;
pub const MAX_ARTIFACT: u64 = 8 * 1024 * 1024 * 1024;
pub const MANIFEST_NAME: &str = "plurx-update-v1.json";
pub const SIGNATURE_NAME: &str = "plurx-update-v1.json.sig";
/// Provision real public update keys only through a reviewed trusted release.
pub const PRODUCTION_ROOTS_JSON: &str = include_str!("../production-trust-roots.json");
#[derive(Clone)]
pub struct TrustKey {
    pub id: String,
    pub public_key: [u8; 32],
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicTrustRootV1 {
    pub id: String,
    pub public_key_hex: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProductionTrustRootsV1 {
    pub schema_version: u32,
    pub repository: String,
    pub keys: Vec<PublicTrustRootV1>,
}
pub fn parse_trust_roots(bytes: &[u8]) -> Result<ProductionTrustRootsV1> {
    if bytes.len() > 65536 {
        return Err(untrusted("embedded production root document exceeds bound"));
    }
    let roots: ProductionTrustRootsV1 = serde_json::from_slice(bytes)
        .map_err(|_| untrusted("invalid embedded production root document"))?;
    if roots.schema_version != 1 || roots.repository != REPOSITORY || roots.keys.len() > 16 {
        return Err(untrusted(
            "unsupported production root schema/repository/count",
        ));
    }
    let mut ids = BTreeSet::new();
    let mut public_keys = BTreeSet::new();
    for key in &roots.keys {
        if key.id.is_empty()
            || key.id.len() > 64
            || !key
                .id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            || !hash(&key.public_key_hex, 64)
            || key.public_key_hex.bytes().all(|b| b == b'0')
            || !ids.insert(&key.id)
            || !public_keys.insert(&key.public_key_hex)
        {
            return Err(untrusted(
                "invalid or duplicate embedded production trust key",
            ));
        }
    }
    Ok(roots)
}
pub fn production_keys() -> Result<Vec<TrustKey>> {
    parse_trust_roots(PRODUCTION_ROOTS_JSON.as_bytes())?
        .keys
        .into_iter()
        .map(|key| {
            let mut public_key = [0; 32];
            hex::decode_to_slice(&key.public_key_hex, &mut public_key)
                .map_err(|_| untrusted("invalid embedded Ed25519 key"))?;
            Ok(TrustKey {
                id: key.id,
                public_key,
            })
        })
        .collect()
}
/// A signed replacement may introduce rotation keys, but must retain the actual
/// authenticating public key for this step. A subsequent new-key-signed release
/// can retire the old key. The caller authenticates the complete candidate bytes.
pub fn bridge_key_ids(
    roots: &ProductionTrustRootsV1,
    signer: &str,
    current: &[TrustKey],
) -> Result<Vec<String>> {
    let old = current
        .iter()
        .find(|key| key.id == signer)
        .ok_or_else(|| untrusted("replacement bridge signer is not currently pinned"))?;
    if !roots
        .keys
        .iter()
        .any(|key| key.id == signer && key.public_key_hex == hex::encode(old.public_key))
    {
        return Err(untrusted(
            "replacement bridge drops or changes its authenticating public key",
        ));
    }
    Ok(roots.keys.iter().map(|key| key.id.clone()).collect())
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    Linux,
    Macos,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    Deb,
    Rpm,
    TarGz,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Component {
    Server,
    Desktop,
    NativeRuntime,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Rollback {
    BinaryCompatible,
    ManualRecovery,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactV1 {
    pub name: String,
    pub platform: Platform,
    pub architecture: Architecture,
    pub format: Format,
    pub component: Component,
    pub package_name: String,
    pub package_version: String,
    pub size: u64,
    pub sha256: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateManifestV1 {
    pub schema_version: u32,
    pub repository: String,
    pub channel: String,
    pub tag: String,
    pub version: String,
    pub sequence: u64,
    pub source_commit: String,
    pub source_tree: String,
    pub packaging_commit: String,
    pub created_unix: u64,
    pub expires_unix: u64,
    pub upgrade_from: Vec<String>,
    pub data_compatibility: String,
    pub cluster_compatibility: String,
    pub minimum_controller: String,
    pub rollback: Rollback,
    pub release_notes_url: String,
    pub artifacts: Vec<ArtifactV1>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptedReleasesV1 {
    pub highest_sequence: u64,
    pub manifests: BTreeMap<String, String>,
}
#[derive(Debug, Clone)]
pub struct VerifiedManifest {
    manifest: UpdateManifestV1,
    digest: String,
    key_id: String,
}
impl VerifiedManifest {
    pub fn manifest(&self) -> &UpdateManifestV1 {
        &self.manifest
    }
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn key_id(&self) -> &str {
        &self.key_id
    }
    pub fn accept(&self, ledger: &mut AcceptedReleasesV1) {
        ledger.highest_sequence = ledger.highest_sequence.max(self.manifest.sequence);
        ledger
            .manifests
            .insert(self.manifest.tag.clone(), self.digest.clone());
    }
}
fn untrusted(message: &str) -> Error {
    Error::new(ErrorCode::UntrustedUpdate, message)
}
fn hash(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}
pub fn asset_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 200
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-+".contains(&c))
        && s != "."
        && s != ".."
        && !s.starts_with('-')
}
pub fn verify_manifest(
    bytes: &[u8],
    signature: &[u8],
    keys: &[TrustKey],
    installed: &InstallationV1,
    ledger: &AcceptedReleasesV1,
    now: u64,
) -> Result<VerifiedManifest> {
    if keys.is_empty() {
        return Err(untrusted("production update trust roots have not been provisioned; use a verified manual bootstrap release"));
    }
    if bytes.is_empty() || bytes.len() > MAX_MANIFEST || signature.len() != 64 {
        return Err(untrusted("invalid signed manifest/signature size"));
    }
    let key = keys
        .iter()
        .find(|k| {
            UnparsedPublicKey::new(&ED25519, k.public_key)
                .verify(bytes, signature)
                .is_ok()
        })
        .ok_or_else(|| untrusted("manifest Ed25519 signature failed"))?;
    if !installed.trust_key_ids.iter().any(|id| id == &key.id) {
        return Err(untrusted(
            "signing key is not part of installed trusted key bridge",
        ));
    }
    let m: UpdateManifestV1 = serde_json::from_slice(bytes)
        .map_err(|_| untrusted("malformed/unsupported signed manifest"))?;
    let version = semver::Version::parse(&m.version)
        .map_err(|_| untrusted("invalid signed application version"))?;
    let current = semver::Version::parse(&installed.current_version)
        .map_err(|_| untrusted("invalid installed version"))?;
    let controller = semver::Version::parse(env!("CARGO_PKG_VERSION"))
        .map_err(|_| untrusted("invalid controller version"))?;
    if m.schema_version != 1
        || m.repository != REPOSITORY
        || m.channel != "stable"
        || m.tag != format!("v{version}")
        || !version.pre.is_empty()
        || version <= current
        || m.sequence == 0
        || m.sequence < ledger.highest_sequence
        || (m.sequence == ledger.highest_sequence && !ledger.manifests.contains_key(&m.tag))
    {
        return Err(untrusted(
            "wrong schema/repository/channel/tag or replay/downgrade",
        ));
    }
    if !hash(&m.source_commit, 40) || !hash(&m.source_tree, 40) || !hash(&m.packaging_commit, 40) {
        return Err(untrusted("invalid signed source provenance"));
    }
    if m.created_unix > now.saturating_add(300)
        || m.expires_unix <= now
        || m.expires_unix <= m.created_unix
        || m.expires_unix - m.created_unix > 90 * 86400
    {
        return Err(untrusted(
            "signed metadata expired or system clock outside validity",
        ));
    }
    if semver::Version::parse(&m.minimum_controller)
        .map_err(|_| untrusted("bad minimum controller"))?
        > controller
    {
        return Err(untrusted(
            "a verified controller bridge/bootstrap upgrade is required",
        ));
    }
    if m.upgrade_from.is_empty()
        || m.upgrade_from.len() > 32
        || !m
            .upgrade_from
            .iter()
            .map(|r| {
                semver::VersionReq::parse(r)
                    .map_err(|_| untrusted("invalid upgrade-from constraint"))
            })
            .collect::<Result<Vec<_>>>()?
            .iter()
            .any(|r| r.matches(&current))
    {
        return Err(untrusted("unsupported upgrade-from version"));
    }
    if m.data_compatibility.is_empty()
        || m.cluster_compatibility.is_empty()
        || m.data_compatibility.len() > 128
        || m.cluster_compatibility.len() > 128
    {
        return Err(untrusted("missing compatibility identities"));
    }
    let notes = reqwest::Url::parse(&m.release_notes_url)
        .map_err(|_| untrusted("invalid release-notes URL"))?;
    if !safe_url(&notes)
        || notes.host_str() != Some("github.com")
        || notes.path() != format!("/{REPOSITORY}/releases/tag/{}", m.tag)
        || notes.query().is_some()
        || notes.fragment().is_some()
    {
        return Err(untrusted(
            "release notes must name the exact trusted GitHub release",
        ));
    }
    if m.artifacts.is_empty() || m.artifacts.len() > 128 {
        return Err(untrusted("invalid artifact count"));
    }
    let mut names = BTreeSet::new();
    for a in &m.artifacts {
        if !asset_name(&a.name)
            || !names.insert(&a.name)
            || a.size == 0
            || a.size > MAX_ARTIFACT
            || !hash(&a.sha256, 64)
            || !asset_name(&a.package_name)
            || a.package_version.is_empty()
            || a.package_version.len() > 128
            || !a
                .package_version
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b".+:~_-".contains(&b))
        {
            return Err(untrusted(
                "invalid/duplicate artifact identity, size or digest",
            ));
        }
        // Exact distro version string is signed; it is not parsed as application semver.
        let platform_ok = matches!(
            (a.platform, a.format),
            (Platform::Linux, Format::Deb | Format::Rpm) | (Platform::Macos, Format::TarGz)
        );
        if !platform_ok {
            return Err(untrusted("artifact platform/format mismatch"));
        }
    }
    let digest = hex::encode(Sha256::digest(bytes));
    if ledger
        .manifests
        .get(&m.tag)
        .is_some_and(|old| old != &digest)
        || (m.sequence == ledger.highest_sequence
            && !ledger.manifests.values().any(|old| old == &digest))
    {
        return Err(untrusted("previously accepted release identity changed"));
    }
    let verified = VerifiedManifest {
        manifest: m,
        digest,
        key_id: key.id.to_string(),
    };
    select_cohort(&verified, installed)?;
    Ok(verified)
}
pub fn select_cohort<'a>(
    verified: &'a VerifiedManifest,
    installed: &InstallationV1,
) -> Result<Vec<&'a ArtifactV1>> {
    let (platform, format, required) = match installed.method {
        InstallMethod::LinuxDeb => (
            Platform::Linux,
            Format::Deb,
            if installed.desktop_installed {
                vec![Component::Server, Component::Desktop]
            } else {
                vec![Component::Server]
            },
        ),
        InstallMethod::LinuxRpm => (
            Platform::Linux,
            Format::Rpm,
            if installed.desktop_installed {
                vec![Component::Server, Component::Desktop]
            } else {
                vec![Component::Server]
            },
        ),
        InstallMethod::MacosNative => (
            Platform::Macos,
            Format::TarGz,
            vec![Component::NativeRuntime],
        ),
        _ => {
            return Err(untrusted(
                "source/external installation requires explicit manual adoption",
            ))
        }
    };
    // Source-built macOS runtimes adopt the same signed universal release cohort.
    // A thin installation never makes a thin release archive eligible.
    let architecture = if installed.method == InstallMethod::MacosNative {
        Architecture::Universal
    } else {
        installed.architecture.clone()
    };
    let mut selected = Vec::new();
    for component in required {
        let mut matches = verified.manifest.artifacts.iter().filter(|a| {
            a.platform == platform
                && a.format == format
                && a.architecture == architecture
                && a.component == component
        });
        let a = matches
            .next()
            .ok_or_else(|| untrusted("incomplete compatible artifact cohort"))?;
        if matches.next().is_some() {
            return Err(untrusted("ambiguous artifact cohort"));
        }
        let package = match component {
            Component::Server => "plurxd",
            Component::Desktop => "plurx-desktop",
            Component::NativeRuntime => "plurx-native",
        };
        if a.package_name != package {
            return Err(untrusted("unexpected package name"));
        }
        selected.push(a);
    }
    if selected
        .windows(2)
        .any(|p| p[0].package_version != p[1].package_version)
    {
        return Err(untrusted("cohort package versions disagree"));
    }
    Ok(selected)
}
fn safe_url(url: &reqwest::Url) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && matches!(
            url.host_str(),
            Some(
                "github.com"
                    | "api.github.com"
                    | "release-assets.githubusercontent.com"
                    | "objects.githubusercontent.com"
            )
        )
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiCacheV1 {
    pub etag: Option<String>,
    pub checked_unix: u64,
    pub retry_unix: u64,
    pub body: Vec<u8>,
}
#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<GitHubAsset>,
}
#[derive(Debug, Deserialize)]
struct GitHubAsset {
    name: String,
    size: u64,
    browser_download_url: String,
}
pub struct PublicReleaseClient {
    client: reqwest::blocking::Client,
}
impl PublicReleaseClient {
    pub fn new() -> Result<Self> {
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .referer(false)
            .user_agent(concat!("plurx-control/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(120))
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= 5 || !safe_url(attempt.url()) {
                    attempt.error("untrusted release redirect")
                } else {
                    attempt.follow()
                }
            }))
            .build()
            .map_err(|_| untrusted("failed to construct credential-free release client"))?;
        Ok(Self { client })
    }
    fn get(&self, url: &str) -> Result<reqwest::blocking::Response> {
        let parsed = reqwest::Url::parse(url).map_err(|_| untrusted("invalid download URL"))?;
        if !safe_url(&parsed) {
            return Err(untrusted("untrusted release host/scheme"));
        }
        self.client
            .get(parsed)
            .send()
            .map_err(|_| Error::new(ErrorCode::Unavailable, "public GitHub download unavailable"))
    }
    pub fn discover(&self, cache: &mut ApiCacheV1, now: u64) -> Result<Vec<String>> {
        if now < cache.retry_unix {
            return Err(Error::new(
                ErrorCode::RateLimited,
                "GitHub backoff active; retry later",
            ));
        }
        let mut request = self.client.get(format!(
            "https://api.github.com/repos/{REPOSITORY}/releases?per_page=30"
        ));
        if let Some(etag) = &cache.etag {
            if etag.len() <= 256 {
                request = request.header(reqwest::header::IF_NONE_MATCH, etag);
            }
        }
        let mut response = request.send().map_err(|_| {
            Error::new(
                ErrorCode::Unavailable,
                "GitHub release discovery unavailable",
            )
        })?;
        cache.checked_unix = now;
        if response.status().as_u16() == 403 || response.status().as_u16() == 429 {
            let seconds = response
                .headers()
                .get("retry-after")
                .and_then(|h| h.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(3600)
                .clamp(60, 86400);
            cache.retry_unix = now.saturating_add(seconds);
            return Err(Error::new(
                ErrorCode::RateLimited,
                "public GitHub rate limit; cached server controls remain available",
            ));
        }
        if response.status().as_u16() != 304 {
            if !response.status().is_success() {
                return Err(Error::new(
                    ErrorCode::Unavailable,
                    "GitHub release API refused request",
                ));
            }
            let etag = response
                .headers()
                .get("etag")
                .and_then(|h| h.to_str().ok())
                .filter(|s| s.len() <= 256)
                .map(str::to_string);
            let body = bounded_read(&mut response, MAX_MANIFEST as u64)?;
            cache.etag = etag;
            cache.body = body;
        }
        let releases: Vec<GitHubRelease> = serde_json::from_slice(&cache.body)
            .map_err(|_| untrusted("malformed GitHub release API response"))?;
        if releases.len() > 30 {
            return Err(untrusted("release list too large"));
        }
        let mut versions = Vec::new();
        for r in releases {
            if r.draft || r.prerelease || r.assets.len() > 256 {
                continue;
            }
            let Some(tag) = r.tag_name.strip_prefix('v') else {
                continue;
            };
            let Ok(v) = semver::Version::parse(tag) else {
                continue;
            };
            if !v.pre.is_empty() {
                continue;
            }
            // Discovery fields are advisory. Signed manifest/cohort remains mandatory.
            if [MANIFEST_NAME, SIGNATURE_NAME].iter().all(|name| {
                r.assets
                    .iter()
                    .filter(|a| {
                        a.name == *name
                            && a.size <= MAX_MANIFEST as u64
                            && a.browser_download_url == release_url(&r.tag_name, name)
                    })
                    .count()
                    == 1
            }) {
                versions.push(v);
            }
        }
        versions.sort();
        versions.reverse();
        Ok(versions.into_iter().map(|v| v.to_string()).collect())
    }
    pub fn manifest(
        &self,
        version: &str,
        i: &InstallationV1,
        ledger: &AcceptedReleasesV1,
        now: u64,
    ) -> Result<(VerifiedManifest, Vec<u8>, Vec<u8>)> {
        let keys = production_keys()?;
        if keys.is_empty() {
            return Err(untrusted(
                "production update roots are not provisioned; no release request sent",
            ));
        }
        let v =
            semver::Version::parse(version).map_err(|_| untrusted("invalid requested version"))?;
        let tag = format!("v{v}");
        let bytes = bounded_read(
            &mut self
                .get(&release_url(&tag, MANIFEST_NAME))?
                .error_for_status()
                .map_err(|_| untrusted("manifest asset unavailable"))?,
            MAX_MANIFEST as u64,
        )?;
        let signature = bounded_read(
            &mut self
                .get(&release_url(&tag, SIGNATURE_NAME))?
                .error_for_status()
                .map_err(|_| untrusted("signature asset unavailable"))?,
            64,
        )?;
        let verified = verify_manifest(&bytes, &signature, &keys, i, ledger, now)?;
        if verified.manifest.version != version {
            return Err(untrusted("requested version differs from signed release"));
        }
        Ok((verified, bytes, signature))
    }
    pub fn stage(
        &self,
        release: &VerifiedManifest,
        i: &InstallationV1,
        dir: &SecureDir,
    ) -> Result<()> {
        for a in select_cohort(release, i)? {
            // Signed size is also a hard streaming bound; bytes are never executed before full verification.
            let mut response = self
                .get(&release_url(&release.manifest.tag, &a.name))?
                .error_for_status()
                .map_err(|_| untrusted("package asset unavailable"))?;
            if response.content_length().is_some_and(|n| n != a.size) {
                return Err(untrusted(
                    "download Content-Length differs from signed size",
                ));
            }
            // Stream to an exclusive root/owner staging inode, then publish after fsync/hash.
            stage_artifact(&mut response, a, dir)?;
        }
        Ok(())
    }
}
pub fn release_url(tag: &str, name: &str) -> String {
    format!("https://github.com/{REPOSITORY}/releases/download/{tag}/{name}")
}
pub fn bounded_read(reader: &mut impl Read, max: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take(max + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max {
        return Err(untrusted("download exceeded hard size limit"));
    }
    Ok(bytes)
}
pub fn stage_artifact(reader: &mut impl Read, asset: &ArtifactV1, dir: &SecureDir) -> Result<()> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let temp = format!(".{}.download", uuid::Uuid::new_v4());
    let cname = std::ffi::CString::new(temp.as_str()).map_err(|_| untrusted("bad staging name"))?;
    // SAFETY: descriptor-bound exclusive leaf, no symlink following.
    let fd = unsafe {
        libc::openat(
            dir.fd(),
            cname.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    let result = (|| {
        let mut total = 0u64;
        let mut digest = Sha256::new();
        let mut buffer = [0u8; 65536];
        loop {
            let n = reader.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            total = total
                .checked_add(n as u64)
                .ok_or_else(|| untrusted("download size overflow"))?;
            if total > asset.size || total > MAX_ARTIFACT {
                return Err(untrusted("download exceeds signed size"));
            }
            digest.update(&buffer[..n]);
            file.write_all(&buffer[..n])?;
        }
        if total != asset.size || hex::encode(digest.finalize()) != asset.sha256 {
            return Err(untrusted("download size/hash mismatch"));
        }
        // SAFETY: owned staging descriptor; immutable to normal consumers after publication.
        if unsafe { libc::fchmod(file.as_raw_fd(), 0o400) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        file.sync_all()?;
        let target =
            std::ffi::CString::new(asset.name.as_str()).map_err(|_| untrusted("bad asset name"))?;
        if !asset_name(&asset.name) {
            return Err(untrusted("bad asset name"));
        }
        // SAFETY: validated leaf names on same authority directory.
        if unsafe { libc::renameat(dir.fd(), cname.as_ptr(), dir.fd(), target.as_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let held = unsafe { libc::dup(dir.fd()) };
        if held < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        unsafe { std::fs::File::from_raw_fd(held) }.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = dir.remove(&temp);
    }
    result
}
pub fn verify_staged(a: &ArtifactV1, dir: &SecureDir) -> Result<()> {
    let mut file = dir.open(&a.name)?;
    if file.metadata()?.len() != a.size {
        return Err(untrusted("staged package size changed"));
    }
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    if hex::encode(hash.finalize()) != a.sha256 {
        return Err(untrusted("staged package digest changed"));
    }
    Ok(())
}
/// Safe archive expansion used by the per-user native installer. Reject links, duplicates,
/// sparse/special/PAX entries and traversal, and bound actual expanded bytes.
pub fn extract_archive(
    reader: impl Read,
    destination: &Path,
    max_unpacked: u64,
) -> Result<Vec<String>> {
    use std::os::unix::fs::OpenOptionsExt;
    if !destination.is_dir() || std::fs::read_dir(destination)?.next().is_some() {
        return Err(untrusted(
            "archive destination must be a fresh empty private directory",
        ));
    }
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(reader));
    let mut seen = BTreeSet::new();
    let mut total = 0u64;
    let mut names = Vec::new();
    for entry in archive.entries()?.raw(true) {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        if path.as_os_str().is_empty()
            || path.is_absolute()
            || !path
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_)))
            || !seen.insert(path.clone())
            || seen.len() > 100000
        {
            return Err(untrusted(
                "archive traversal/duplicate/member count violation",
            ));
        }
        let kind = entry.header().entry_type();
        if !kind.is_file() && !kind.is_dir() {
            return Err(untrusted(
                "archive links, special and extended entries are forbidden",
            ));
        }
        let dest = destination.join(&path);
        if kind.is_dir() {
            std::fs::create_dir_all(&dest)?;
            continue;
        }
        total = total
            .checked_add(entry.size())
            .ok_or_else(|| untrusted("expanded size overflow"))?;
        if total > max_unpacked {
            return Err(untrusted("archive expanded size exceeded"));
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mode = if entry.header().mode()? & 0o111 != 0 {
            0o500
        } else {
            0o400
        };
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(mode)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&dest)?;
        let copied = std::io::copy(&mut entry, &mut file)?;
        if copied != file.metadata()?.len() {
            return Err(untrusted("archive short write"));
        }
        file.sync_all()?;
        names.push(path.to_string_lossy().into_owned());
    }
    std::fs::File::open(destination)?.sync_all()?;
    Ok(names)
}
