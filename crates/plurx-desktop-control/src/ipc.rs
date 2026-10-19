//! One peer-bound HTTP/1 connection. Reconnect is never implicit and no TCP fallback exists.
use crate::{
    installation::InstallationV1,
    process::{self, KernelProcess},
    protocol::*,
    supervisor, Error, Result,
};
use serde::{de::DeserializeOwned, Serialize};
use std::{
    io::{Read, Write},
    os::unix::{
        fs::{FileTypeExt, MetadataExt},
        net::UnixStream,
    },
    time::Duration,
};

pub struct BoundDaemon {
    stream: UnixStream,
    peer: KernelProcess,
    pub identity: DaemonIdentityV1,
    installation: InstallationV1,
}
impl BoundDaemon {
    pub fn connect(i: &InstallationV1) -> Result<Self> {
        let before = supervisor::observe(i)?;
        let expected = before.process.ok_or_else(|| {
            Error::new(
                ErrorCode::IdentityMismatch,
                "no exact supervisor process to bind",
            )
        })?;
        let parent = i
            .paths
            .socket
            .parent()
            .ok_or_else(|| Error::new(ErrorCode::InvalidInstallation, "socket parent"))?;
        // Ancestor protection is independent of the record. No desktop-writable Linux socket parent.
        crate::secure_fs::open_dir(parent, i.service_uid)?;
        let m = std::fs::symlink_metadata(&i.paths.socket)?;
        if !m.file_type().is_socket()
            || m.uid() != i.service_uid
            || (cfg!(target_os = "macos") && m.mode() & 0o077 != 0)
        {
            return Err(Error::new(
                ErrorCode::IdentityMismatch,
                "socket ownership/type/permissions mismatch",
            ));
        }
        let stream = UnixStream::connect(&i.paths.socket)?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        stream.set_write_timeout(Some(Duration::from_secs(2)))?;
        let peer = process::socket_peer(&stream)?;
        if peer != expected {
            return Err(Error::new(
                ErrorCode::IdentityMismatch,
                "socket kernel peer is not current supervisor process",
            ));
        }
        // Public identity is requested only after kernel/supervisor proof. Still never trust its strings as authority.
        let mut bound = Self {
            stream,
            peer,
            identity: DaemonIdentityV1 {
                schema_version: 1,
                installation_id: String::new(),
                process: ProcessIdentityV1 {
                    pid: 0,
                    uid: 0,
                    start_generation: String::new(),
                    nonce: String::new(),
                },
                node_id: String::new(),
                instance_id: String::new(),
                version: String::new(),
                build: String::new(),
                browser_origin: String::new(),
                supervisor: before.supervisor,
                ready: false,
            },
            installation: i.clone(),
        };
        let identity: DaemonIdentityV1 = bound.request("GET", IDENTITY_PATH, &[])?;
        if identity.schema_version != 1
            || identity.installation_id != i.installation_id
            || identity.process.pid != bound.peer.pid
            || identity.process.uid != bound.peer.uid
            || identity.process.start_generation != bound.peer.generation
            || identity.process.nonce.len() < 16
            || identity.process.nonce.len() > 256
            || identity.supervisor != bound.identity.supervisor
        {
            return Err(Error::new(
                ErrorCode::IdentityMismatch,
                "daemon handshake does not bind the verified process/installation",
            ));
        }
        crate::installation::validate_browser_url(&identity.browser_origin)?;
        bound.identity = identity;
        bound.revalidate()?;
        Ok(bound)
    }
    pub fn peer(&self) -> &KernelProcess {
        &self.peer
    }
    pub fn revalidate(&self) -> Result<()> {
        let observed = supervisor::observe(&self.installation)?;
        if observed.process.as_ref() != Some(&self.peer)
            || observed.supervisor != self.identity.supervisor
            || process::socket_peer(&self.stream)? != self.peer
        {
            return Err(Error::new(
                ErrorCode::IdentityMismatch,
                "supervisor/peer generation changed; reconnect and authenticate anew",
            ));
        }
        Ok(())
    }
    pub fn prepare(
        &mut self,
        operation_id: &str,
        operation: Operation,
        target_version: Option<String>,
    ) -> Result<ShutdownPreparedV1> {
        self.revalidate()?;
        self.stream
            .set_read_timeout(Some(Duration::from_secs(75)))?;
        let request = ShutdownPrepareRequestV1 {
            schema_version: 1,
            operation_id: operation_id.into(),
            operation,
            process_nonce: self.identity.process.nonce.clone(),
            confirm_interruption: true,
            target_version,
        };
        let p: ShutdownPreparedV1 = self.post(PREPARE_PATH, &request)?;
        if p.schema_version != 1
            || p.operation_id != operation_id
            || p.process_nonce != self.identity.process.nonce
            || p.owner_capability.len() < 16
            || p.owner_capability.len() > 4096
            || p.generation.is_empty()
            || p.generation.len() > 256
            || p.expires_unix_ms <= crate::unix_time()?.saturating_mul(1000)
        {
            return Err(Error::new(
                ErrorCode::UnsafeShutdown,
                "invalid/expired shutdown preparation",
            ));
        }
        Ok(p)
    }
    pub fn commit(&mut self, p: ShutdownPreparedV1) -> Result<ShutdownCommittedV1> {
        self.revalidate()?;
        self.stream
            .set_read_timeout(Some(Duration::from_secs(75)))?;
        let request = ShutdownCommitRequestV1 {
            schema_version: 1,
            operation_id: p.operation_id,
            process_nonce: p.process_nonce,
            generation: p.generation,
            owner_capability: p.owner_capability,
        };
        let committed: ShutdownCommittedV1 = self.post(COMMIT_PATH, &request)?;
        if committed.schema_version != 1
            || !committed.committed
            || committed.operation_id != request.operation_id
            || committed.process_nonce != request.process_nonce
            || committed.generation != request.generation
        {
            return Err(Error::new(
                ErrorCode::UnsafeShutdown,
                "shutdown commitment does not match preparation",
            ));
        }
        Ok(committed)
    }
    pub fn cancel(&mut self, p: ShutdownPreparedV1) -> Result<()> {
        self.revalidate()?;
        let request = ShutdownCommitRequestV1 {
            schema_version: 1,
            operation_id: p.operation_id,
            process_nonce: p.process_nonce,
            generation: p.generation,
            owner_capability: p.owner_capability,
        };
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Cancel {
            schema_version: u32,
            cancelled: bool,
        }
        let c: Cancel = self.post(CANCEL_PATH, &request)?;
        if c.schema_version != 1 || !c.cancelled {
            return Err(Error::new(
                ErrorCode::UnsafeShutdown,
                "daemon did not cancel preparation",
            ));
        }
        Ok(())
    }
    fn post<T: Serialize, R: DeserializeOwned>(&mut self, path: &str, request: &T) -> Result<R> {
        self.request("POST", path, &serde_json::to_vec(request)?)
    }
    fn request<R: DeserializeOwned>(&mut self, method: &str, path: &str, body: &[u8]) -> Result<R> {
        if body.len() > 1024 * 1024
            || !matches!(
                path,
                IDENTITY_PATH | PREPARE_PATH | COMMIT_PATH | CANCEL_PATH | STATUS_PATH
            )
        {
            return Err(Error::new(
                ErrorCode::InvalidRequest,
                "unsupported local request",
            ));
        }
        write!(self.stream, "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n", body.len())?;
        self.stream.write_all(body)?;
        self.stream.flush()?;
        let (code, bytes) = read_http(&mut self.stream)?;
        if !(200..300).contains(&code) {
            let error: ControlErrorV1 = serde_json::from_slice(&bytes).unwrap_or(ControlErrorV1 {
                code: if code == 401 {
                    ErrorCode::AuthorizationRequired
                } else if code == 403 {
                    ErrorCode::AuthorizationDenied
                } else {
                    ErrorCode::UnsafeShutdown
                },
                message: "local daemon refused operation".into(),
                retryable: false,
            });
            return Err(Error::new(error.code, error.message));
        }
        Ok(serde_json::from_slice(&bytes)?)
    }
}
/// Strict framing avoids consuming bytes from the next response on the held connection.
pub fn read_http(reader: &mut impl Read) -> Result<(u16, Vec<u8>)> {
    let mut header = Vec::new();
    let mut byte = [0u8; 1];
    while !header.ends_with(b"\r\n\r\n") {
        if header.len() >= 16384 {
            return Err(Error::new(
                ErrorCode::InvalidRequest,
                "IPC headers too large",
            ));
        }
        reader.read_exact(&mut byte)?;
        header.push(byte[0]);
    }
    let text = std::str::from_utf8(&header)
        .map_err(|_| Error::new(ErrorCode::InvalidRequest, "non-ASCII HTTP header"))?;
    let mut lines = text.split("\r\n");
    let status = lines
        .next()
        .ok_or_else(|| Error::new(ErrorCode::InvalidRequest, "missing status"))?;
    let mut parts = status.split_whitespace();
    if parts.next() != Some("HTTP/1.1") {
        return Err(Error::new(
            ErrorCode::InvalidRequest,
            "unsupported HTTP version",
        ));
    }
    let code: u16 = parts
        .next()
        .ok_or_else(|| Error::new(ErrorCode::InvalidRequest, "missing HTTP code"))?
        .parse()
        .map_err(|_| Error::new(ErrorCode::InvalidRequest, "bad HTTP code"))?;
    let mut length = None;
    for line in lines.filter(|l| !l.is_empty()) {
        let (key, value) = line
            .split_once(':')
            .ok_or_else(|| Error::new(ErrorCode::InvalidRequest, "bad HTTP header"))?;
        if key.eq_ignore_ascii_case("transfer-encoding") {
            return Err(Error::new(
                ErrorCode::InvalidRequest,
                "chunked IPC is not accepted",
            ));
        }
        if key.eq_ignore_ascii_case("content-length") {
            if length.is_some() {
                return Err(Error::new(
                    ErrorCode::InvalidRequest,
                    "duplicate content length",
                ));
            }
            length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .map_err(|_| Error::new(ErrorCode::InvalidRequest, "bad content length"))?,
            );
        }
    }
    let length = length
        .ok_or_else(|| Error::new(ErrorCode::InvalidRequest, "missing bounded content length"))?;
    if length > 1024 * 1024 {
        return Err(Error::new(ErrorCode::InvalidRequest, "IPC body too large"));
    }
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes)?;
    Ok((code, bytes))
}
