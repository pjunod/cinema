//! Public HTTPS identity, independent of sharing pins and replicated settings.
mod admin;
mod transport;
pub(crate) use admin::{Control, SaveError, Update};
pub(crate) use transport::{Prepared, Serving};

use anyhow::{bail, Context, Result};
use base64::Engine;
use plurx_core::config::https::HttpsConfig;
use rustls::{
    pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer},
    ServerConfig,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    net::{Ipv4Addr, Ipv6Addr},
    path::Path,
    sync::Arc,
};
use x509_parser::{
    extensions::GeneralName,
    prelude::{FromDer, X509Certificate},
};
use zeroize::Zeroizing;

pub const MANAGED_IDENTITY_FILE: &str = "identity.pem";
const MATERIAL_LIMIT: u64 = 1024 * 1024;
const YEAR: i64 = 365 * 24 * 60 * 60;

#[derive(Clone, Debug, Serialize)]
pub struct CertificateFacts {
    pub mode: &'static str,
    /// SHA-256 of the complete active leaf certificate DER (not the SPKI).
    pub sha256: String,
    pub sans: Vec<String>,
    pub expires_at_seconds: i64,
}
pub struct Identity {
    pub config: Arc<ServerConfig>,
    pub facts: CertificateFacts,
    chain: Vec<CertificateDer<'static>>,
}
impl Identity {
    /// With create=false this is read-only; an absent managed identity is valid
    /// first-enable intent. Disabled HTTPS never opens retained material.
    pub fn load(
        settings: &HttpsConfig,
        data_dir: &Path,
        now: i64,
        create: bool,
    ) -> Result<Option<Self>> {
        if !settings.enabled {
            return Ok(None);
        }
        if settings.generated() {
            let root =
                std::fs::canonicalize(data_dir).context("HTTPS data directory is unavailable")?;
            let data_dir = root.as_path();
            let directory = data_dir.join("https");
            let path = directory.join(MANAGED_IDENTITY_FILE);
            if create {
                prepare_managed_directory(&directory)?;
            } else if std::fs::symlink_metadata(&directory).is_ok() {
                validate_managed_directory(&directory)?;
            }
            let material = match read_regular(&path, true, true) {
                Ok(material) => material,
                Err(error) if error.downcast_ref::<std::io::Error>().is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) => {
                    if !create {
                        // A settings Save may inspect storage without creating an identity.
                        let metadata = std::fs::metadata(data_dir).context("HTTPS data directory is unavailable")?;
                        anyhow::ensure!(metadata.is_dir() && !metadata.permissions().readonly(), "HTTPS data directory is not writable");
                        return Ok(None);
                    }
                    let material = generate_bundle(settings, now)?;
                    publish_managed(&directory, &path, &material)?;
                    read_regular(&path, true, true)?
                }
                Err(error) => return Err(error.context("managed HTTPS identity is unavailable; restore it or stop and explicitly replace identity.pem, then re-trust")),
            };
            let blocks = pem_blocks(&material)?;
            anyhow::ensure!(
                blocks.len() == 2 && blocks[0].0 == "CERTIFICATE" && blocks[1].0 == "PRIVATE KEY",
                "managed HTTPS identity must contain exactly one certificate and private key"
            );
            let identity = Self::from_pem(&blocks[0].1, &blocks[1].1, now, "generated")?;
            anyhow::ensure!(identity.facts.sans == settings.sans, "managed HTTPS SANs differ from desired SANs; stop, edit intent/revision, back up/remove identity.pem, restart and re-trust");
            Ok(Some(identity))
        } else {
            let chain = read_regular(&settings.cert_file, false, false)
                .context("cannot read supplied HTTPS certificate chain")?;
            let key = read_regular(&settings.key_file, false, false)
                .context("cannot read supplied HTTPS private key")?;
            Self::from_pem(&chain, &key, now, "supplied").map(Some)
        }
    }
    fn from_pem(chain: &[u8], key: &[u8], now: i64, mode: &'static str) -> Result<Self> {
        anyhow::ensure!(now >= 0, "HTTPS requires a valid system clock");
        let blocks = pem_blocks(chain)?;
        anyhow::ensure!(
            !blocks.is_empty() && blocks.iter().all(|b| b.0 == "CERTIFICATE"),
            "HTTPS chain must contain only leaf-first CERTIFICATE blocks"
        );
        let chain: Vec<_> = blocks
            .iter()
            .map(|b| CertificateDer::from_pem_slice(&b.1).context("invalid HTTPS certificate PEM"))
            .collect::<Result<_>>()?;
        let key_blocks = pem_blocks(key)?;
        anyhow::ensure!(
            key_blocks.len() == 1
                && matches!(
                    key_blocks[0].0.as_str(),
                    "PRIVATE KEY" | "RSA PRIVATE KEY" | "EC PRIVATE KEY"
                ),
            "HTTPS key must contain one unencrypted supported private key"
        );
        let key = PrivateKeyDer::from_pem_slice(key).context("invalid HTTPS private key PEM")?;
        let facts = validate_chain(&chain, now, mode)?;
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut config = ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .context("HTTPS protocol configuration failed")?
            .with_no_client_auth()
            .with_single_cert(chain.clone(), key)
            .context(
                "HTTPS certificate and private key do not form a supported matching identity",
            )?;
        config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        Ok(Self {
            config: Arc::new(config),
            facts,
            chain,
        })
    }
    /// Derive the public export only from the loaded chain, never the bundle.
    pub fn public_chain_pem(&self) -> Vec<u8> {
        let mut output = String::new();
        for certificate in &self.chain {
            output.push_str("-----BEGIN CERTIFICATE-----\n");
            let encoded = base64::engine::general_purpose::STANDARD.encode(certificate);
            for line in encoded.as_bytes().chunks(64) {
                output.push_str(std::str::from_utf8(line).expect("base64 ASCII"));
                output.push('\n');
            }
            output.push_str("-----END CERTIFICATE-----\n");
        }
        output.into_bytes()
    }
}

/// Strictly split PEM: whitespace is the only permitted surrounding material.
fn pem_blocks(input: &[u8]) -> Result<Vec<(String, Vec<u8>)>> {
    let mut text = std::str::from_utf8(input)
        .context("HTTPS material is not PEM text")?
        .trim();
    let mut blocks = Vec::new();
    while !text.is_empty() {
        let label_end = text
            .strip_prefix("-----BEGIN ")
            .and_then(|s| s.find("-----").map(|i| i + 11))
            .context("unexpected material outside HTTPS PEM blocks")?;
        let label = &text[11..label_end];
        anyhow::ensure!(
            matches!(
                label,
                "CERTIFICATE" | "PRIVATE KEY" | "RSA PRIVATE KEY" | "EC PRIVATE KEY"
            ),
            "unsupported HTTPS PEM block"
        );
        let marker = format!("-----END {label}-----");
        let end = text.find(&marker).context("incomplete HTTPS PEM block")? + marker.len();
        blocks.push((label.to_owned(), text.as_bytes()[..end].to_vec()));
        anyhow::ensure!(blocks.len() <= 32, "too many HTTPS PEM blocks");
        text = text[end..].trim();
    }
    Ok(blocks)
}
fn parsed(der: &[u8]) -> Result<X509Certificate<'_>> {
    let (remaining, certificate) = X509Certificate::from_der(der)
        .map_err(|_| anyhow::anyhow!("invalid HTTPS certificate DER"))?;
    anyhow::ensure!(remaining.is_empty(), "trailing HTTPS certificate DER");
    Ok(certificate)
}
fn validate_chain(
    chain: &[CertificateDer<'_>],
    now: i64,
    mode: &'static str,
) -> Result<CertificateFacts> {
    let parsed = chain
        .iter()
        .map(|c| parsed(c))
        .collect::<Result<Vec<_>>>()?;
    for (index, certificate) in parsed.iter().enumerate() {
        anyhow::ensure!(certificate.validity().not_before.timestamp() <= now && now < certificate.validity().not_after.timestamp(), "HTTPS certificate is expired or not yet valid; correct the clock or explicitly replace/reload it");
        if index == 0 {
            anyhow::ensure!(
                !certificate.is_ca(),
                "HTTPS leaf must be an end-entity certificate"
            );
            if let Some(eku) = certificate
                .extended_key_usage()
                .map_err(|_| anyhow::anyhow!("invalid HTTPS extended key usage"))?
            {
                anyhow::ensure!(
                    eku.value.server_auth || eku.value.any,
                    "HTTPS certificate does not permit server authentication"
                );
            }
            if let Some(usage) = certificate
                .key_usage()
                .map_err(|_| anyhow::anyhow!("invalid HTTPS key usage"))?
            {
                anyhow::ensure!(
                    usage.value.digital_signature(),
                    "HTTPS certificate does not permit handshake signatures"
                );
            }
        } else {
            anyhow::ensure!(
                certificate.is_ca(),
                "HTTPS chain intermediates must be CA certificates"
            );
        }
        if let Some(issuer) = parsed.get(index + 1) {
            anyhow::ensure!(
                certificate.issuer() == issuer.subject(),
                "HTTPS certificate chain is not leaf-first"
            );
            certificate
                .verify_signature(Some(issuer.public_key()))
                .map_err(|_| {
                    anyhow::anyhow!("HTTPS chain contains an unrelated or invalid issuer")
                })?;
        } else if certificate.issuer() == certificate.subject() {
            certificate
                .verify_signature(None)
                .map_err(|_| anyhow::anyhow!("invalid self-signed HTTPS certificate"))?;
        }
    }
    let leaf = &parsed[0];
    let mut sans = Vec::new();
    if let Some(extension) = leaf
        .subject_alternative_name()
        .map_err(|_| anyhow::anyhow!("invalid HTTPS SAN extension"))?
    {
        for san in &extension.value.general_names {
            match san {
                GeneralName::DNSName(name) => sans.push(name.to_ascii_lowercase()),
                GeneralName::IPAddress(bytes) if bytes.len() == 4 => sans.push(
                    Ipv4Addr::from(<[u8; 4]>::try_from(*bytes).expect("IPv4 SAN")).to_string(),
                ),
                GeneralName::IPAddress(bytes) if bytes.len() == 16 => sans.push(
                    Ipv6Addr::from(<[u8; 16]>::try_from(*bytes).expect("IPv6 SAN")).to_string(),
                ),
                _ => {}
            }
        }
    }
    sans.sort();
    sans.dedup();
    anyhow::ensure!(
        !sans.is_empty(),
        "HTTPS certificate requires DNS or IP SANs"
    );
    Ok(CertificateFacts {
        mode,
        sha256: hex::encode(Sha256::digest(&chain[0])),
        sans,
        expires_at_seconds: leaf.validity().not_after.timestamp(),
    })
}
fn read_regular(path: &Path, nofollow: bool, private: bool) -> Result<Zeroizing<Vec<u8>>> {
    let file = if nofollow {
        plurx_core::fs_secure::open_read_nofollow_blocking(path)?
    } else {
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            let mut options = std::fs::OpenOptions::new();
            options
                .read(true)
                .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_NOCTTY);
            options.open(path)?
        }
        #[cfg(windows)]
        {
            use std::path::{Component, Prefix};
            let mut components = path.components();
            anyhow::ensure!(
                matches!(components.next(), Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)))
                    && matches!(components.next(), Some(Component::RootDir)),
                "Windows HTTPS material requires a regular local disk path; device, pipe and UNC namespaces are unsupported"
            );
            // Reject reparse points before traversal: following an apparently
            // ordinary cert-manager link could otherwise open a named pipe.
            plurx_core::fs_secure::open_read_nofollow_blocking(path)?
        }
    };
    let metadata = file.metadata()?;
    anyhow::ensure!(
        metadata.is_file() && metadata.len() <= MATERIAL_LIMIT,
        "HTTPS material must be a regular file no larger than 1 MiB"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        anyhow::ensure!(
            !private || metadata.permissions().mode() & 0o077 == 0,
            "managed HTTPS identity must have mode 0600"
        );
    }
    #[cfg(not(unix))]
    let _ = private;
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(MATERIAL_LIMIT + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() as u64 == metadata.len() && bytes.len() as u64 <= MATERIAL_LIMIT,
        "HTTPS material changed or exceeded its read limit"
    );
    Ok(bytes)
}
fn prepare_managed_directory(directory: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = std::fs::DirBuilder::new();
        builder.mode(0o700);
        match builder.create(directory) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
    }
    #[cfg(windows)]
    {
        std::fs::create_dir_all(directory)?;
        plurx_core::fs_secure::harden_private_path_blocking(directory, true)?;
    }
    validate_managed_directory(directory)
}
fn validate_managed_directory(directory: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let owned = plurx_core::fs_secure::open_directory_nofollow_blocking(directory)
            .context("managed HTTPS directory must not contain symlinks")?;
        anyhow::ensure!(
            owned.metadata()?.permissions().mode() & 0o077 == 0,
            "managed HTTPS directory must have mode 0700"
        );
    }
    #[cfg(windows)]
    {
        anyhow::ensure!(
            !std::fs::symlink_metadata(directory)?
                .file_type()
                .is_symlink(),
            "managed HTTPS directory must not be a symlink"
        );
    }
    Ok(())
}
fn generate_bundle(settings: &HttpsConfig, now: i64) -> Result<Zeroizing<Vec<u8>>> {
    let key = rcgen::KeyPair::generate().context("cannot generate HTTPS signing key")?;
    let mut params = rcgen::CertificateParams::new(settings.sans.clone())
        .context("invalid generated HTTPS SANs")?;
    params.not_before = time::OffsetDateTime::from_unix_timestamp(
        now.checked_sub(300).context("invalid HTTPS clock")?,
    )?;
    params.not_after = time::OffsetDateTime::from_unix_timestamp(
        now.checked_add(YEAR).context("invalid HTTPS clock")?,
    )?;
    params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
    params.key_usages = vec![rcgen::KeyUsagePurpose::DigitalSignature];
    let certificate = params
        .self_signed(&key)
        .context("cannot generate HTTPS certificate")?;
    let mut material = Zeroizing::new(certificate.pem().into_bytes());
    let key_pem = Zeroizing::new(key.serialize_pem());
    material.extend_from_slice(key_pem.as_bytes());
    Ok(material)
}
fn publish_managed(directory: &Path, path: &Path, material: &[u8]) -> Result<()> {
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    #[cfg(windows)]
    plurx_core::fs_secure::harden_private_path_blocking(temporary.path(), false)?;
    temporary.write_all(material)?;
    temporary.as_file().sync_all()?;
    match temporary.persist_noclobber(path) {
        Ok(_) => {}
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => bail!(
            "cannot atomically publish HTTPS identity: {}",
            error.error.kind()
        ),
    }
    #[cfg(unix)]
    std::fs::File::open(directory)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    const NOW: i64 = 1_800_000_000;
    fn settings() -> HttpsConfig {
        HttpsConfig {
            enabled: true,
            ..Default::default()
        }
        .normalize(std::net::IpAddr::V4(Ipv4Addr::UNSPECIFIED))
        .expect("synthetic HTTPS fixture")
    }
    fn directory() -> tempfile::TempDir {
        tempfile::tempdir_in(
            std::fs::canonicalize(std::env::temp_dir()).expect("synthetic HTTPS fixture"),
        )
        .expect("synthetic HTTPS fixture")
    }
    #[test]
    fn generated_identity_is_immutable_across_restart_disable_and_export() {
        let dir = directory();
        let settings = settings();
        assert!(Identity::load(&settings, dir.path(), NOW, false)
            .expect("synthetic HTTPS fixture")
            .is_none());
        assert!(!dir.path().join("https").exists());
        let first = Identity::load(&settings, dir.path(), NOW, true)
            .expect("synthetic HTTPS fixture")
            .expect("synthetic HTTPS fixture");
        let original = std::fs::read(dir.path().join("https").join(MANAGED_IDENTITY_FILE))
            .expect("synthetic HTTPS fixture");
        let mut disabled = settings.clone();
        disabled.enabled = false;
        assert!(Identity::load(&disabled, dir.path(), NOW, true)
            .expect("synthetic HTTPS fixture")
            .is_none());
        let second = Identity::load(&settings, dir.path(), NOW + 1000, true)
            .expect("synthetic HTTPS fixture")
            .expect("synthetic HTTPS fixture");
        assert_eq!(first.facts.sha256, second.facts.sha256);
        assert_eq!(
            original,
            std::fs::read(dir.path().join("https").join(MANAGED_IDENTITY_FILE))
                .expect("synthetic HTTPS fixture")
        );
        let export = pem_blocks(&first.public_chain_pem()).expect("synthetic HTTPS fixture");
        assert_eq!(export.len(), 1);
        assert_eq!(export[0].0, "CERTIFICATE");
        let cert = CertificateDer::from_pem_slice(&export[0].1).expect("synthetic HTTPS fixture");
        assert_eq!(hex::encode(Sha256::digest(cert)), first.facts.sha256);
        assert!(Identity::load(&settings, dir.path(), NOW + YEAR + 1, true).is_err());
        let mut changed = settings.clone();
        changed.sans.push("media1.lan".into());
        changed.sans.sort();
        assert!(Identity::load(&changed, dir.path(), NOW, false).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(dir.path().join("https"))
                    .expect("synthetic HTTPS fixture")
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            assert_eq!(
                std::fs::metadata(dir.path().join("https").join(MANAGED_IDENTITY_FILE))
                    .expect("synthetic HTTPS fixture")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }
    #[test]
    fn concurrent_generation_preserves_one_complete_identity() {
        let dir = directory();
        let root = dir.path().to_owned();
        let barrier = Arc::new(std::sync::Barrier::new(6));
        let tasks: Vec<_> = (0..6)
            .map(|_| {
                let root = root.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    Identity::load(&settings(), &root, NOW, true)
                        .expect("synthetic HTTPS fixture")
                        .expect("synthetic HTTPS fixture")
                        .facts
                        .sha256
                })
            })
            .collect();
        let hashes: Vec<_> = tasks
            .into_iter()
            .map(|t| t.join().expect("synthetic HTTPS fixture"))
            .collect();
        assert!(hashes.iter().all(|h| h == &hashes[0]));
    }
    #[test]
    fn damaged_managed_material_is_never_replaced() {
        let dir = directory();
        let settings = settings();
        Identity::load(&settings, dir.path(), NOW, true).expect("synthetic HTTPS fixture");
        let path = dir.path().join("https").join(MANAGED_IDENTITY_FILE);
        std::fs::write(&path, "partial private identity").expect("synthetic HTTPS fixture");
        assert!(Identity::load(&settings, dir.path(), NOW, true).is_err());
        assert_eq!(
            std::fs::read(&path).expect("synthetic HTTPS fixture"),
            b"partial private identity"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::{symlink, PermissionsExt};
            std::fs::remove_file(&path).expect("synthetic HTTPS fixture");
            let target = dir.path().join("key.pem");
            std::fs::write(&target, "unchanged").expect("synthetic HTTPS fixture");
            symlink(&target, &path).expect("synthetic HTTPS fixture");
            assert!(Identity::load(&settings, dir.path(), NOW, true).is_err());
            std::fs::remove_file(&path).expect("synthetic HTTPS fixture");
            std::fs::set_permissions(
                path.parent().expect("synthetic HTTPS fixture"),
                std::fs::Permissions::from_mode(0o755),
            )
            .expect("synthetic HTTPS fixture");
            assert!(Identity::load(&settings, dir.path(), NOW, true).is_err());
        }
    }
    #[cfg(unix)]
    #[test]
    fn supplied_fifo_and_symlinked_fifo_are_rejected_without_blocking() {
        use std::{
            ffi::CString,
            os::unix::{ffi::OsStrExt, fs::symlink},
        };
        let dir = directory();
        let path = dir.path().join("pipe");
        let name = CString::new(path.as_os_str().as_bytes()).expect("synthetic HTTPS fixture");
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let link = dir.path().join("pipe-link");
        symlink(&path, &link).expect("synthetic HTTPS fixture");
        for path in [path, link] {
            let (send, receive) = std::sync::mpsc::channel();
            // The observer must stay outside the worker: a blocking-open
            // regression must fail on the deadline rather than hang in join.
            // Only that failure path detaches a blocked worker, whose lifetime
            // is then bounded by this test process. Successful work is joined.
            let worker = std::thread::spawn(move || {
                let _ = send.send(read_regular(&path, false, false).is_err());
            });
            let refused = receive
                .recv_timeout(std::time::Duration::from_secs(2))
                .expect("FIFO inspection must not block");
            worker.join().expect("FIFO inspection worker must settle");
            assert!(refused);
        }
    }
    #[test]
    fn supplied_material_requires_matching_server_identity_and_regular_bounded_files() {
        let dir = directory();
        let managed = Identity::load(&settings(), dir.path(), NOW, true)
            .expect("synthetic HTTPS fixture")
            .expect("synthetic HTTPS fixture");
        let cert = dir.path().join("cert.pem");
        let key = dir.path().join("key.pem");
        std::fs::write(&cert, managed.public_chain_pem()).expect("synthetic HTTPS fixture");
        std::fs::write(
            &key,
            rcgen::KeyPair::generate()
                .expect("synthetic HTTPS fixture")
                .serialize_pem(),
        )
        .expect("synthetic HTTPS fixture");
        let supplied = HttpsConfig {
            cert_file: cert.clone(),
            key_file: key.clone(),
            ..settings()
        };
        assert!(Identity::load(&supplied, dir.path(), NOW, false).is_err());
        std::fs::write(&cert, vec![b'x'; MATERIAL_LIMIT as usize + 1])
            .expect("synthetic HTTPS fixture");
        assert!(Identity::load(&supplied, dir.path(), NOW, false).is_err());
        assert!(pem_blocks(
            b"-----BEGIN CERTIFICATE-----\nAA==\n-----END CERTIFICATE-----\nprivate junk"
        )
        .is_err());
        let mut disabled = supplied;
        disabled.enabled = false;
        std::fs::remove_file(cert).expect("synthetic HTTPS fixture");
        assert!(Identity::load(&disabled, dir.path(), NOW, false)
            .expect("synthetic HTTPS fixture")
            .is_none());
    }
    #[tokio::test]
    async fn generated_tls_uses_local_trust_and_rejects_unknown_trust_and_wrong_hostname() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let dir = directory();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("synthetic HTTPS fixture")
            .as_secs() as i64;
        let identity = Identity::load(&settings(), dir.path(), now, true)
            .expect("synthetic HTTPS fixture")
            .expect("synthetic HTTPS fixture");
        let root =
            reqwest::Certificate::from_der(&identity.chain[0]).expect("synthetic HTTPS fixture");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("synthetic HTTPS fixture");
        let addr = listener.local_addr().expect("synthetic HTTPS fixture");
        let server = tokio::spawn(async move {
            for _ in 0..3 {
                let (stream, _) = listener.accept().await.expect("synthetic HTTPS fixture");
                if let Ok(mut stream) = tokio_rustls::TlsAcceptor::from(identity.config.clone())
                    .accept(stream)
                    .await
                {
                    let mut request = [0_u8; 1024];
                    let _ = stream.read(&mut request).await;
                    let _ = stream
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                        )
                        .await;
                    let _ = stream.shutdown().await;
                }
            }
        });
        let trusted = reqwest::Client::builder()
            .no_proxy()
            .http1_only()
            .add_root_certificate(root.clone())
            .timeout(std::time::Duration::from_secs(3))
            .build()
            .expect("synthetic HTTPS fixture");
        assert_eq!(
            trusted
                .get(format!("https://localhost:{}", addr.port()))
                .send()
                .await
                .expect("synthetic HTTPS fixture")
                .text()
                .await
                .expect("synthetic HTTPS fixture"),
            "ok"
        );
        let untrusted = reqwest::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_secs(3))
            .build()
            .expect("synthetic HTTPS fixture");
        assert!(untrusted
            .get(format!("https://localhost:{}", addr.port()))
            .send()
            .await
            .is_err());
        let wrong = reqwest::Client::builder()
            .no_proxy()
            .add_root_certificate(root)
            .resolve("wrong.invalid", addr)
            .timeout(std::time::Duration::from_secs(3))
            .build()
            .expect("synthetic HTTPS fixture");
        assert!(wrong
            .get(format!("https://wrong.invalid:{}", addr.port()))
            .send()
            .await
            .is_err());
        server.await.expect("synthetic HTTPS fixture");
    }
    #[cfg(unix)]
    #[test]
    fn generated_identity_accepts_symlink_data_root_but_refuses_insecure_managed_directory() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let dir = directory();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).expect("synthetic HTTPS fixture");
        let link = dir.path().join("data");
        symlink(&real, &link).expect("synthetic HTTPS fixture");
        let identity = Identity::load(&settings(), &link, NOW, true)
            .expect("synthetic HTTPS fixture")
            .expect("synthetic HTTPS fixture");
        assert_eq!(
            identity.facts.sha256,
            Identity::load(&settings(), &real, NOW, false)
                .expect("synthetic HTTPS fixture")
                .expect("synthetic HTTPS fixture")
                .facts
                .sha256
        );
        std::fs::set_permissions(real.join("https"), std::fs::Permissions::from_mode(0o755))
            .expect("synthetic HTTPS fixture");
        assert!(Identity::load(&settings(), &link, NOW, false).is_err());
    }
    #[tokio::test]
    async fn supplied_private_ca_chain_is_normally_trusted_and_cert_manager_symlinks_work() {
        let dir = directory();
        let key = rcgen::KeyPair::generate().expect("synthetic HTTPS fixture");
        let mut ca = rcgen::CertificateParams::new(vec![]).expect("synthetic HTTPS fixture");
        ca.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        ca.key_usages = vec![
            rcgen::KeyUsagePurpose::KeyCertSign,
            rcgen::KeyUsagePurpose::CrlSign,
        ];
        let root = ca.self_signed(&key).expect("synthetic HTTPS fixture");
        let leaf_key = rcgen::KeyPair::generate().expect("synthetic HTTPS fixture");
        let mut params = rcgen::CertificateParams::new(vec!["localhost".into()])
            .expect("synthetic HTTPS fixture");
        params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
        params.key_usages = vec![rcgen::KeyUsagePurpose::DigitalSignature];
        let leaf = params
            .signed_by(&leaf_key, &rcgen::Issuer::from_params(&ca, &key))
            .expect("synthetic HTTPS fixture");
        let cert_path = dir.path().join("chain.pem");
        let key_path = dir.path().join("key.pem");
        std::fs::write(&cert_path, format!("{}{}", leaf.pem(), root.pem()))
            .expect("synthetic HTTPS fixture");
        std::fs::write(&key_path, leaf_key.serialize_pem()).expect("synthetic HTTPS fixture");
        #[cfg(unix)]
        let (cert_path, key_path) = {
            use std::os::unix::fs::symlink;
            let c = dir.path().join("cert-link");
            let k = dir.path().join("key-link");
            symlink(cert_path, &c).expect("synthetic HTTPS fixture");
            symlink(key_path, &k).expect("synthetic HTTPS fixture");
            (c, k)
        };
        let supplied = HttpsConfig {
            cert_file: cert_path,
            key_file: key_path,
            ..settings()
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("synthetic HTTPS fixture")
            .as_secs() as i64;
        let identity = Identity::load(&supplied, dir.path(), now, false)
            .expect("synthetic HTTPS fixture")
            .expect("synthetic HTTPS fixture");
        assert_eq!(identity.facts.mode, "supplied");
        assert_eq!(
            pem_blocks(&identity.public_chain_pem())
                .expect("synthetic HTTPS fixture")
                .len(),
            2
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("synthetic HTTPS fixture");
        let address = listener.local_addr().expect("synthetic HTTPS fixture");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("synthetic HTTPS fixture");
            tokio_rustls::TlsAcceptor::from(identity.config)
                .accept(stream)
                .await
                .expect("synthetic HTTPS fixture");
        });
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(root.der().clone())
            .expect("synthetic HTTPS fixture");
        let client = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .expect("synthetic HTTPS fixture")
        .with_root_certificates(roots)
        .with_no_client_auth();
        let stream = tokio::net::TcpStream::connect(address)
            .await
            .expect("synthetic HTTPS fixture");
        tokio_rustls::TlsConnector::from(Arc::new(client))
            .connect(
                rustls::pki_types::ServerName::try_from("localhost")
                    .expect("synthetic HTTPS fixture"),
                stream,
            )
            .await
            .expect("synthetic HTTPS fixture");
        server.await.expect("synthetic HTTPS fixture");
        let mut client_only = params.clone();
        client_only.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ClientAuth];
        let client_cert = client_only
            .self_signed(&leaf_key)
            .expect("synthetic HTTPS fixture");
        assert!(Identity::from_pem(
            client_cert.pem().as_bytes(),
            leaf_key.serialize_pem().as_bytes(),
            now,
            "supplied"
        )
        .is_err());
    }
}
