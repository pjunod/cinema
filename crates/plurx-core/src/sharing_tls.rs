//! Node-local sharing TLS. A pin authorizes one key, while certificate and
//! handshake signatures, validity and server usage remain mandatory.
use crate::error::StoreError;
use rustls::{
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    pki_types::{CertificateDer, PrivatePkcs8KeyDer, ServerName, UnixTime},
    CertificateError, ClientConfig, DigitallySignedStruct, ServerConfig, SignatureScheme,
};
use sha2::{Digest, Sha256};
use std::{path::Path, sync::Arc, time::Duration};
use subtle::ConstantTimeEq;
use x509_parser::prelude::{FromDer, X509Certificate};

const YEAR: i64 = 365 * 24 * 60 * 60;
const RENEW_BEFORE: i64 = 30 * 24 * 60 * 60;
const KEY_FILE: &str = "sharing-tls.key";
const CERT_FILE: &str = "sharing-tls.der";
fn unavailable() -> StoreError {
    StoreError::Identity(
        "sharing TLS unavailable; repair the node-local certificate and key".into(),
    )
}
fn malformed() -> rustls::Error {
    rustls::Error::InvalidCertificate(CertificateError::BadEncoding)
}
fn parsed(der: &[u8]) -> Result<X509Certificate<'_>, rustls::Error> {
    let (rest, cert) = X509Certificate::from_der(der).map_err(|_| malformed())?;
    if !rest.is_empty() {
        return Err(malformed());
    }
    Ok(cert)
}
/// SHA-256 over complete DER SubjectPublicKeyInfo, rather than certificate bytes.
pub fn fingerprint(der: &[u8]) -> Result<String, StoreError> {
    Ok(hex::encode(Sha256::digest(
        parsed(der).map_err(|_| unavailable())?.public_key().raw,
    )))
}

#[derive(Debug)]
struct PinVerifier {
    pin: [u8; 32],
    provider: Arc<rustls::crypto::CryptoProvider>,
}
impl ServerCertVerifier for PinVerifier {
    fn verify_server_cert(
        &self,
        end: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _name: &ServerName<'_>,
        _ocsp: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        // This protocol provisions a self-signed, node-local leaf. A chain is
        // not an alternative authority to the leaf's approved SPKI.
        if !intermediates.is_empty() {
            return Err(malformed());
        }
        let cert = parsed(end)?;
        let got = Sha256::digest(cert.public_key().raw);
        if !bool::from(got.as_slice().ct_eq(&self.pin)) {
            return Err(rustls::Error::InvalidCertificate(
                CertificateError::ApplicationVerificationFailure,
            ));
        }
        let seconds = i64::try_from(now.as_secs()).map_err(|_| malformed())?;
        if seconds < cert.validity().not_before.timestamp() {
            return Err(rustls::Error::InvalidCertificate(
                CertificateError::NotValidYet,
            ));
        }
        if seconds >= cert.validity().not_after.timestamp() {
            return Err(rustls::Error::InvalidCertificate(CertificateError::Expired));
        }
        // A trust anchor's own signature is not checked by path validation.
        // This protocol requires the provisioned leaf to be self-signed too.
        cert.verify_signature(None)
            .map_err(|_| rustls::Error::InvalidCertificate(CertificateError::BadSignature))?;
        let end_cert = webpki::EndEntityCert::try_from(end).map_err(|_| malformed())?;
        let anchor = webpki::anchor_from_trusted_cert(end).map_err(|_| malformed())?;
        end_cert
            .verify_for_usage(
                self.provider.signature_verification_algorithms.all,
                &[anchor],
                &[],
                now,
                webpki::KeyUsage::server_auth(),
                None,
                None,
            )
            .map_err(|_| rustls::Error::InvalidCertificate(CertificateError::BadSignature))?;
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            signature,
            &self.provider.signature_verification_algorithms,
        )
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            signature,
            &self.provider.signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}
/// A dedicated client config: there is no general invalid-certificate mode.
pub fn pinned_client(pin: &str) -> Result<ClientConfig, StoreError> {
    if pin.len() != 64
        || !pin
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(unavailable());
    }
    let bytes = hex::decode(pin).map_err(|_| unavailable())?;
    let pin: [u8; 32] = bytes.try_into().map_err(|_| unavailable())?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    Ok(ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|_| unavailable())?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinVerifier { pin, provider }))
        .with_no_client_auth())
}

/// Operator-provisioned egress for one qualified network namespace. Interface
/// binding is used on Linux hosts; bridge containers bind their own address.
/// Host forwarding/firewall qualification is still required for a no-WAN claim.
pub enum PinnedEgress {
    Interface(String),
    LocalAddress(std::net::IpAddr),
}

/// Dial one already validated numeric Tailscale address. This does no DNS,
/// inherits no HTTP proxy, and sends no application bytes. The caller must
/// verify both source UUIDs on the returned stream before sending a secret.
pub async fn dial_numeric_peer(
    address: std::net::SocketAddr,
    ts_fqdn: &str,
    pin: &str,
    egress: &PinnedEgress,
) -> Result<tokio_rustls::client::TlsStream<tokio::net::TcpStream>, StoreError> {
    crate::sharing::validate_tailnet_name(ts_fqdn).map_err(|_| unavailable())?;
    if !crate::sharing::is_tailnet_address(address.ip()) || address.port() == 0 {
        return Err(unavailable());
    }
    let config = pinned_client(pin)?;
    let name = ServerName::try_from(ts_fqdn.to_owned()).map_err(|_| unavailable())?;
    let socket = bound_tcp_socket(address, egress)?;
    tokio::time::timeout(Duration::from_secs(5), async {
        let stream = socket.connect(address).await.map_err(|_| unavailable())?;
        stream.set_nodelay(true).map_err(|_| unavailable())?;
        tokio_rustls::TlsConnector::from(Arc::new(config))
            .connect(name, stream)
            .await
            .map_err(|_| unavailable())
    })
    .await
    .map_err(|_| unavailable())?
}

/// Bind before connecting; also used for the fixed Tailscale DNS TCP retry.
pub(crate) fn bound_tcp_socket(
    address: std::net::SocketAddr,
    egress: &PinnedEgress,
) -> Result<tokio::net::TcpSocket, StoreError> {
    let socket = if address.is_ipv4() {
        tokio::net::TcpSocket::new_v4()
    } else {
        tokio::net::TcpSocket::new_v6()
    }
    .map_err(|_| unavailable())?;
    match egress {
        PinnedEgress::LocalAddress(ip) => {
            if ip.is_unspecified() || ip.is_multicast() || ip.is_ipv4() != address.is_ipv4() {
                return Err(unavailable());
            }
            socket
                .bind(std::net::SocketAddr::new(*ip, 0))
                .map_err(|_| unavailable())?;
        }
        PinnedEgress::Interface(interface) => {
            if interface.is_empty() || interface.len() > 15 || interface.as_bytes().contains(&0) {
                return Err(unavailable());
            }
            #[cfg(target_os = "linux")]
            socket
                .bind_device(Some(interface.as_bytes()))
                .map_err(|_| unavailable())?;
            #[cfg(not(target_os = "linux"))]
            return Err(unavailable());
        }
    }
    Ok(socket)
}

/// A node-local leaf and server config. Only status fields may be serialized.
pub struct NodeTls {
    pub certificate: Vec<u8>,
    pub spki_sha256: String,
    pub expires_at_seconds: i64,
    pub config: Arc<ServerConfig>,
}
fn read_bounded(path: &Path, max: u64) -> Result<Vec<u8>, StoreError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|_| unavailable())?;
    if !metadata.is_file() || metadata.len() > max {
        return Err(unavailable());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(unavailable());
        }
    }
    std::fs::read(path).map_err(|_| unavailable())
}
fn publish_certificate(dir: &Path, der: &[u8]) -> Result<(), StoreError> {
    use std::io::Write;
    let mut file = tempfile::NamedTempFile::new_in(dir).map_err(|_| unavailable())?;
    file.write_all(der).map_err(|_| unavailable())?;
    file.as_file().sync_all().map_err(|_| unavailable())?;
    file.persist(dir.join(CERT_FILE))
        .map_err(|_| unavailable())?;
    // Make the rename durable before announcing the replacement.
    #[cfg(unix)]
    std::fs::File::open(dir)
        .and_then(|f| f.sync_all())
        .map_err(|_| unavailable())?;
    Ok(())
}
fn certificate(key: &rcgen::KeyPair, now: i64) -> Result<Vec<u8>, StoreError> {
    let mut params = rcgen::CertificateParams::new(vec!["cinema-sharing.invalid".into()])
        .map_err(|_| unavailable())?;
    params.not_before =
        time::OffsetDateTime::from_unix_timestamp(now.checked_sub(3600).ok_or_else(unavailable)?)
            .map_err(|_| unavailable())?;
    params.not_after =
        time::OffsetDateTime::from_unix_timestamp(now.checked_add(YEAR).ok_or_else(unavailable)?)
            .map_err(|_| unavailable())?;
    params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
    Ok(params
        .self_signed(key)
        .map_err(|_| unavailable())?
        .der()
        .to_vec())
}
impl NodeTls {
    /// Operator provisioning refuses an existing identity, including a damaged one.
    pub fn initialize(dir: &Path, now: i64) -> Result<Self, StoreError> {
        if dir.join(KEY_FILE).symlink_metadata().is_ok()
            || dir.join(CERT_FILE).symlink_metadata().is_ok()
        {
            return Err(unavailable());
        }
        Self::open(dir, now)
    }
    /// Called by the sharing subsystem only. Its failures never stop local HTTP.
    /// Calling again before expiry renews atomically with the same private key;
    /// a runtime swaps `config` only after this returns successfully.
    pub fn open(dir: &Path, now: i64) -> Result<Self, StoreError> {
        if now < 0 {
            return Err(unavailable());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)
                .map_err(|_| unavailable())?;
            let metadata = std::fs::symlink_metadata(dir).map_err(|_| unavailable())?;
            if !metadata.is_dir() || metadata.permissions().mode() & 0o022 != 0 {
                return Err(unavailable());
            }
        }
        #[cfg(windows)]
        {
            std::fs::create_dir_all(dir).map_err(|_| unavailable())?;
            crate::fs_secure::harden_private_path_blocking(dir, true).map_err(|_| unavailable())?;
        }
        let key_path = dir.join(KEY_FILE);
        let cert_path = dir.join(CERT_FILE);
        if !key_path.exists() {
            // A missing key beside a certificate is a lost identity, not first use.
            if cert_path.exists() {
                return Err(unavailable());
            }
            let key = rcgen::KeyPair::generate().map_err(|_| unavailable())?;
            let pem = zeroize::Zeroizing::new(key.serialize_pem());
            use std::io::Write;
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(&key_path) {
                Ok(mut file) => {
                    file.write_all(pem.as_bytes())
                        .and_then(|()| file.sync_all())
                        .map_err(|_| unavailable())?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err(unavailable()),
            }
        }
        #[cfg(windows)]
        crate::fs_secure::harden_private_path_blocking(&key_path, false)
            .map_err(|_| unavailable())?;
        let pem = zeroize::Zeroizing::new(read_bounded(&key_path, 8192)?);
        let key = rcgen::KeyPair::from_pem(std::str::from_utf8(&pem).map_err(|_| unavailable())?)
            .map_err(|_| unavailable())?;
        let mut der = if cert_path.exists() {
            read_bounded(&cert_path, 32768)?
        } else {
            Vec::new()
        };
        if !der.is_empty() {
            let expected = certificate(&key, now)?;
            if fingerprint(&der)? != fingerprint(&expected)? {
                return Err(unavailable());
            }
        }
        let renew = der.is_empty()
            || parsed(&der)
                .map_err(|_| unavailable())?
                .validity()
                .not_after
                .timestamp()
                .checked_sub(now)
                .ok_or_else(unavailable)?
                <= RENEW_BEFORE;
        if renew {
            der = certificate(&key, now)?;
            publish_certificate(dir, &der)?;
        }
        let expires = parsed(&der)
            .map_err(|_| unavailable())?
            .validity()
            .not_after
            .timestamp();
        // Check validity and signatures using the same verifier B uses.
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let pin: [u8; 32] =
            Sha256::digest(parsed(&der).map_err(|_| unavailable())?.public_key().raw).into();
        PinVerifier {
            pin,
            provider: provider.clone(),
        }
        .verify_server_cert(
            &CertificateDer::from(der.clone()),
            &[],
            &ServerName::try_from("cinema-sharing.invalid").map_err(|_| unavailable())?,
            &[],
            UnixTime::since_unix_epoch(Duration::from_secs(now as u64)),
        )
        .map_err(|_| unavailable())?;
        let config = ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|_| unavailable())?
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(der.clone())],
                PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
            )
            .map_err(|_| unavailable())?;
        Ok(Self {
            spki_sha256: fingerprint(&der)?,
            certificate: der,
            expires_at_seconds: expires,
            config: Arc::new(config),
        })
    }
}

/// A live node identity. A failed renewal leaves the last verified server
/// configuration in place; peers still reject it once its validity expires.
pub struct LiveNodeTls {
    directory: std::path::PathBuf,
    current: std::sync::RwLock<NodeTls>,
}
impl LiveNodeTls {
    pub fn open(directory: &Path, now: i64) -> Result<Self, StoreError> {
        Ok(Self {
            directory: directory.to_path_buf(),
            current: std::sync::RwLock::new(NodeTls::open(directory, now)?),
        })
    }
    pub fn config(&self) -> Result<Arc<ServerConfig>, StoreError> {
        Ok(self
            .current
            .read()
            .map_err(|_| unavailable())?
            .config
            .clone())
    }
    pub fn status(&self) -> Result<(String, i64), StoreError> {
        let node = self.current.read().map_err(|_| unavailable())?;
        Ok((node.spki_sha256.clone(), node.expires_at_seconds))
    }
    /// Blocking filesystem work; the daemon calls this on its blocking pool.
    pub fn renew(&self, now: i64) -> Result<(), StoreError> {
        let mut current = self.current.write().map_err(|_| unavailable())?;
        let replacement = NodeTls::open(&self.directory, now)?;
        if replacement.spki_sha256 != current.spki_sha256 {
            return Err(unavailable());
        }
        if replacement.certificate != current.certificate {
            *current = replacement;
        }
        Ok(())
    }
}

pub type SharingTlsStream = tokio_rustls::server::TlsStream<tokio::net::TcpStream>;
pub type PinnedPeerStream = tokio_rustls::client::TlsStream<tokio::net::TcpStream>;
type AcceptedTls = (SharingTlsStream, std::net::SocketAddr);
type PendingTls = std::pin::Pin<Box<dyn std::future::Future<Output = Option<AcceptedTls>> + Send>>;

/// Bounded concurrent TLS acceptance for the dedicated peer router. TLS does
/// not authenticate a recipient; every peer request still needs Cinema authority.
pub struct SharingTlsListener {
    listener: tokio::net::TcpListener,
    identity: Arc<LiveNodeTls>,
    pending: tokio::sync::Mutex<futures_util::stream::FuturesUnordered<PendingTls>>,
}
impl SharingTlsListener {
    pub fn new(listener: tokio::net::TcpListener, identity: Arc<LiveNodeTls>) -> Self {
        Self {
            listener,
            identity,
            pending: Default::default(),
        }
    }
    pub async fn accept(&self) -> std::io::Result<AcceptedTls> {
        use futures_util::StreamExt;
        let mut pending = self.pending.lock().await;
        loop {
            tokio::select! {
                accepted = self.listener.accept(), if pending.len() < 64 => {
                    let (stream, remote) = accepted?;
                    stream.set_nodelay(true)?;
                    let config = self.identity.config().map_err(std::io::Error::other)?;
                    pending.push(Box::pin(async move {
                        let acceptor = tokio_rustls::TlsAcceptor::from(config);
                        match tokio::time::timeout(Duration::from_secs(4), acceptor.accept(stream)).await {
                            Ok(Ok(tls)) => Some((tls, remote)),
                            _ => None,
                        }
                    }));
                }
                completed = pending.next(), if !pending.is_empty() => {
                    if let Some(Some(accepted)) = completed {
                        return Ok(accepted);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const NOW: i64 = 1_800_000_000;
    #[tokio::test]
    async fn sharing_tls_numeric_dial_rejects_non_tailnet_addresses_and_names() {
        let egress = PinnedEgress::LocalAddress("127.0.0.1".parse().expect("synthetic bind"));
        for ip in [
            "127.0.0.1",
            "192.168.1.1",
            "8.8.8.8",
            "100.63.255.255",
            "100.128.0.1",
            "::1",
            "2001:db8::1",
            "::ffff:100.64.0.1",
        ] {
            let address = std::net::SocketAddr::new(ip.parse().expect("fixture IP"), 32443);
            assert!(
                dial_numeric_peer(address, "source.example.ts.net", &"0".repeat(64), &egress)
                    .await
                    .is_err()
            );
        }
        for name in [
            "source.ts.net",
            "source.example.com",
            "Source.example.ts.net",
            "source.example.ts.net.",
            "https://source.example.ts.net",
            "source..example.ts.net",
        ] {
            assert!(dial_numeric_peer(
                "100.64.0.1:32443".parse().expect("tailnet address"),
                name,
                &"0".repeat(64),
                &egress
            )
            .await
            .is_err());
        }
    }
    #[test]
    fn sharing_tls_hot_reload_retains_configuration_on_failure() {
        let dir = tempfile::tempdir().expect("synthetic TLS directory");
        let node = LiveNodeTls::open(dir.path(), NOW).expect("live identity");
        let first = node.config().expect("verified configuration");
        node.renew(NOW + 1).expect("not yet due");
        assert!(Arc::ptr_eq(
            &first,
            &node.config().expect("unchanged configuration")
        ));
        node.renew(NOW + YEAR - RENEW_BEFORE + 1).expect("renew");
        let renewed = node.config().expect("new verified configuration");
        assert!(!Arc::ptr_eq(&first, &renewed));
        std::fs::remove_file(dir.path().join(KEY_FILE)).expect("simulate lost test key");
        assert!(node.renew(NOW + YEAR).is_err());
        assert!(Arc::ptr_eq(
            &renewed,
            &node.config().expect("retained configuration")
        ));
    }
    #[tokio::test]
    async fn sharing_tls_silent_handshake_does_not_block_another_peer() {
        let dir = tempfile::tempdir().expect("synthetic TLS directory");
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        let identity = Arc::new(LiveNodeTls::open(dir.path(), now).expect("live node"));
        let pin = identity.status().expect("node status").0;
        let tcp = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("disposable listener");
        let address = tcp.local_addr().expect("synthetic address");
        let listener = SharingTlsListener::new(tcp, identity);
        let server = tokio::spawn(async move { listener.accept().await.expect("verified peer") });
        let silent = tokio::net::TcpStream::connect(address)
            .await
            .expect("silent synthetic peer");
        let tcp = tokio::net::TcpStream::connect(address)
            .await
            .expect("active synthetic peer");
        let connector =
            tokio_rustls::TlsConnector::from(Arc::new(pinned_client(&pin).expect("pin")));
        let peer = connector.connect(
            ServerName::try_from("source.example.ts.net").expect("name"),
            tcp,
        );
        let _verified = tokio::time::timeout(Duration::from_secs(2), peer)
            .await
            .expect("silent peer cannot occupy the accept loop")
            .expect("pinned handshake");
        let _accepted = server.await.expect("listener task");
        drop(silent);
    }
    #[test]
    fn sharing_tls_renewal_preserves_pin_and_refuses_lost_key() {
        let dir = tempfile::tempdir().expect("synthetic node TLS directory");
        let first = NodeTls::open(dir.path(), NOW).expect("provision node identity");
        let renewed =
            NodeTls::open(dir.path(), NOW + YEAR - RENEW_BEFORE + 1).expect("renew with same key");
        assert_ne!(first.certificate, renewed.certificate);
        assert_eq!(first.spki_sha256, renewed.spki_sha256);
        assert!(renewed.expires_at_seconds > first.expires_at_seconds);
        std::fs::remove_file(dir.path().join(KEY_FILE)).expect("lose test key");
        assert!(NodeTls::open(dir.path(), NOW + YEAR).is_err());
        assert!(!dir.path().join(KEY_FILE).exists());
    }
    #[test]
    fn sharing_tls_failed_renewal_preserves_identity_and_recovers() {
        let dir = tempfile::tempdir().expect("synthetic node TLS directory");
        let first = NodeTls::initialize(dir.path(), NOW).expect("provision identity");
        assert!(NodeTls::initialize(dir.path(), NOW).is_err());
        let key_path = dir.path().join(KEY_FILE);
        let original = zeroize::Zeroizing::new(std::fs::read(&key_path).expect("synthetic key"));
        std::fs::write(&key_path, b"damaged synthetic key").expect("simulate key read failure");
        let renewal_time = NOW + YEAR - RENEW_BEFORE + 1;
        assert!(NodeTls::open(dir.path(), renewal_time).is_err());
        assert_eq!(
            std::fs::read(dir.path().join(CERT_FILE)).expect("retained certificate"),
            first.certificate,
        );
        std::fs::write(&key_path, &*original).expect("repair original test key");
        let recovered = NodeTls::open(dir.path(), renewal_time).expect("renew after repair");
        assert_eq!(first.spki_sha256, recovered.spki_sha256);
        assert!(recovered.expires_at_seconds > first.expires_at_seconds);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&key_path)
                    .expect("key mode")
                    .permissions()
                    .mode()
                    & 0o077,
                0
            );
            assert_eq!(
                std::fs::metadata(dir.path().join(CERT_FILE))
                    .expect("cert mode")
                    .permissions()
                    .mode()
                    & 0o077,
                0
            );
        }
    }
    #[tokio::test]
    async fn sharing_tls_real_handshake_sends_no_application_bytes_for_wrong_pin() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let dir = tempfile::tempdir().expect("synthetic TLS directory");
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        let node = NodeTls::open(dir.path(), now).expect("synthetic node");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("disposable loopback listener");
        let address = listener.local_addr().expect("loopback address");
        let acceptor = tokio_rustls::TlsAcceptor::from(node.config.clone());
        let server = tokio::spawn(async move {
            let mut delivered = 0;
            for _ in 0..2 {
                let (stream, _) = listener.accept().await.expect("synthetic connection");
                if let Ok(mut tls) = acceptor.accept(stream).await {
                    let mut bytes = [0u8; 32];
                    let count = tls
                        .read(&mut bytes)
                        .await
                        .expect("synthetic application body");
                    if count > 0 {
                        delivered += 1;
                        assert_eq!(&bytes[..count], b"synthetic-claim");
                    }
                }
            }
            delivered
        });
        let name = ServerName::try_from("source.example.ts.net").expect("synthetic name");
        let wrong = tokio_rustls::TlsConnector::from(Arc::new(
            pinned_client(&"0".repeat(64)).expect("synthetic wrong pin"),
        ));
        let tcp = tokio::net::TcpStream::connect(address)
            .await
            .expect("wrong-pin TCP connect");
        assert!(wrong.connect(name.clone(), tcp).await.is_err());
        let right = tokio_rustls::TlsConnector::from(Arc::new(
            pinned_client(&node.spki_sha256).expect("synthetic pin"),
        ));
        let tcp = tokio::net::TcpStream::connect(address)
            .await
            .expect("pinned TCP connect");
        let mut tls = right
            .connect(name, tcp)
            .await
            .expect("verified pinned handshake");
        tls.write_all(b"synthetic-claim")
            .await
            .expect("post-verification application bytes");
        tls.shutdown().await.expect("close synthetic stream");
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), server)
                .await
                .expect("bounded handshake test")
                .expect("synthetic server"),
            1
        );
    }

    #[test]
    fn sharing_tls_pin_validity_and_server_usage_remain_mandatory() {
        let key = rcgen::KeyPair::generate().expect("synthetic key");
        let der = certificate(&key, NOW).expect("synthetic leaf");
        let pin: [u8; 32] =
            Sha256::digest(parsed(&der).expect("synthetic leaf").public_key().raw).into();
        let verifier = PinVerifier {
            pin,
            provider: Arc::new(rustls::crypto::ring::default_provider()),
        };
        let leaf = CertificateDer::from(der);
        let name = ServerName::try_from("different.source.ts.net").expect("synthetic hostname");
        let check = |v: &PinVerifier, n: i64| {
            v.verify_server_cert(
                &leaf,
                &[],
                &name,
                &[],
                UnixTime::since_unix_epoch(Duration::from_secs(n as u64)),
            )
        };
        assert!(check(&verifier, NOW).is_ok());
        assert!(check(&verifier, NOW - 3601).is_err());
        assert!(check(&verifier, NOW - 3599).is_ok());
        assert!(check(&verifier, NOW + YEAR).is_err());
        let wrong = PinVerifier {
            pin: [0; 32],
            provider: verifier.provider.clone(),
        };
        assert!(check(&wrong, NOW).is_err());
        let mut corrupt = leaf.as_ref().to_vec();
        *corrupt.last_mut().expect("signature byte") ^= 1;
        assert!(verifier
            .verify_server_cert(
                &CertificateDer::from(corrupt),
                &[],
                &name,
                &[],
                UnixTime::since_unix_epoch(Duration::from_secs(NOW as u64)),
            )
            .is_err());
        let mut params = rcgen::CertificateParams::new(vec!["different.source.ts.net".into()])
            .expect("synthetic params");
        params.not_before =
            time::OffsetDateTime::from_unix_timestamp(NOW - 60).expect("synthetic time");
        params.not_after =
            time::OffsetDateTime::from_unix_timestamp(NOW + YEAR).expect("synthetic time");
        params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ClientAuth];
        let client_leaf = params
            .self_signed(&key)
            .expect("synthetic client certificate");
        assert!(verifier
            .verify_server_cert(
                client_leaf.der(),
                &[],
                &name,
                &[],
                UnixTime::since_unix_epoch(Duration::from_secs(NOW as u64))
            )
            .is_err());
    }
}
