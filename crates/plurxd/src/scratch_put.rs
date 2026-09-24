//! FFmpeg's HLS muxer writes through this receiver, not straight to disk.
//!
//! The rolling scratch budget can only be enforced at a point where Rust sees
//! a write before it lands. The native copy writer has that point
//! (`copyseg::SessionDir::publish_file`). FFmpeg's `-f hls` muxer did not: it
//! opened and wrote its own segment and playlist files, so the only thing
//! plurx could do was measure a directory afterwards, and a measurement can
//! discover an overrun but never prevent one. That is why direct and
//! transcoded sessions had to reserve their whole per-session ceiling.
//!
//! This module gives the muxer an owned write boundary without changing the
//! muxer. `-method PUT` makes FFmpeg upload every object over HTTP. The
//! playlist tags, segment boundaries, init segments, discontinuities and
//! timestamps are the same ones it writes to a file, byte for byte. The
//! upload goes to a loopback listener owned by the session, which
//! authorizes each piece of body against the scratch ledger before writing
//! it into the session directory. When the budget refuses, the receiver
//! stops reading the socket. The bytes then wait in kernel socket buffers
//! instead of the scratch directory, and the flow controller's own hold
//! suspends FFmpeg.
//!
//! What stays the same for everything downstream:
//!
//! * Objects land under their FFmpeg names in the session directory through
//!   a temporary file and a rename, so a reader sees an object either
//!   absent or complete, as with `-hls_flags temp_file`.
//! * A playlist is only renamed into place once every object it names has
//!   been renamed into place. FFmpeg opens a separate connection per object
//!   and does not wait for a response, so arrival order alone would not
//!   give that guarantee. File output gives it, and nothing downstream is
//!   written to cope with a playlist that names a missing segment.
//! * Every request holds a scratch writer registration for as long as it
//!   can write, so retirement's writer barrier covers these writes the same
//!   way it covers the copy reader. FFmpeg's exit is not the end of its
//!   writes here: a completed upload can still be queued on the socket.
//!   [`PutSink::drain`] is the exact point after which it is.
//!
//! Each producer attempt writes to its own lane (`/<token>/<lane>/<name>`).
//! A request on a lane older than the newest one seen is refused and never
//! committed, so a late upload from a replaced attempt cannot overwrite its
//! successor's objects.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

use crate::copyseg::WriteGrants;
use crate::scratch_ledger::{ScratchWrite, ScratchWriter};

// Each piece of body is authorized for exactly its own length, just before
// it is written. A larger block would refuse a piece the budget could still
// hold, and the allocation's own headroom already keeps a steady writer from
// renegotiating its ceiling on every piece.

/// How often a starved writer asks the budget again.
const GRANT_WAIT_POLL: Duration = Duration::from_millis(250);
/// How long a writer waits for the budget before a session that has never
/// published gives up. Same rule as the copy writer: before the first
/// playlist nobody can drain anything, so waiting has no end. Afterwards a
/// starved writer is the flow controller's hold and waits as long as the
/// session lives.
const PREPUBLICATION_GRANT_WAIT: Duration = Duration::from_secs(120);
/// How long a playlist waits for the objects it names. FFmpeg writes a
/// segment completely before it writes the playlist that names it, so in
/// practice this is the time the receiver takes to finish that segment.
/// Expiry drops this one playlist version. The next one supersedes it.
const PLAYLIST_ORDER_WAIT: Duration = Duration::from_secs(30);
const MAX_REQUEST_HEAD: usize = 16 * 1024;
const MAX_PLAYLIST_BYTES: usize = 8 << 20;
const PIECE_BYTES: usize = 64 * 1024;

/// Called when a writer is refused a grant, so the flow controller can hold
/// the producer now instead of at its next scheduled evaluation.
pub(crate) type StarvedHook = Box<dyn Fn() + Send + Sync>;

/// The upload endpoint for one rolling session's FFmpeg producers.
///
/// Owned by the session. Dropping it stops accepting, wakes every request
/// that is waiting, and lets each one remove its temporary file and release
/// its writer registration.
pub(crate) struct PutSink {
    shared: Arc<Shared>,
    acceptor: tokio::task::JoinHandle<()>,
    /// Asks the acceptor to sweep the queue. The sweep runs inside the
    /// acceptor, so there is only ever one party accepting and nothing to
    /// race: an earlier version handed a lock back and forth between the
    /// acceptor and `drain`, and under load the acceptor could take it back
    /// before `drain` had queued for it, then wait for a connection that
    /// would never come while holding it.
    drains: tokio::sync::mpsc::UnboundedSender<tokio::sync::oneshot::Sender<()>>,
}

struct Shared {
    dir: PathBuf,
    token: String,
    addr: SocketAddr,
    /// A duplicate of the accepting socket. A drain sweep accepts through
    /// it without waiting for readiness, which is what makes "nothing is
    /// queued" an observation rather than a guess.
    drain_listener: std::net::TcpListener,
    grants: Option<WriteGrants>,
    closed: AtomicBool,
    in_flight: AtomicUsize,
    idle: tokio::sync::Notify,
    /// Wakes playlist commits waiting on a segment, and requests waiting on
    /// the sink closing.
    changed: tokio::sync::Notify,
    /// Serializes the lane check and the rename that follows it.
    commit: tokio::sync::Mutex<()>,
    state: Mutex<State>,
    starved: OnceLock<StarvedHook>,
    next_request: AtomicU64,
}

#[derive(Default)]
struct State {
    lane: u32,
    /// Every object name this lane has renamed into place. Pruning by the
    /// segment GC does not remove a name: the playlist still lists it, and
    /// it did land.
    committed: HashSet<String>,
    /// `(media entries, ENDLIST)` of the last playlist renamed into place in
    /// this lane. An EVENT playlist only grows, so a smaller one is stale.
    playlist_rank: Option<(usize, bool)>,
    published: bool,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// Count a connection before it is handed to its task, so `drain` can
    /// never see zero between an accept and the task that serves it.
    fn serve(self: &Arc<Self>, stream: tokio::net::TcpStream) {
        self.in_flight.fetch_add(1, Ordering::AcqRel);
        let shared = Arc::clone(self);
        tokio::spawn(async move {
            let _flight = Flight(Arc::clone(&shared));
            serve_connection(&shared, stream).await;
        });
    }
}

struct Flight(Arc<Shared>);

impl Drop for Flight {
    fn drop(&mut self) {
        if self.0.in_flight.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.0.idle.notify_waiters();
        }
    }
}

impl PutSink {
    /// Bind a loopback listener for one session directory.
    ///
    /// `grants` is `None` only in tests of the transport itself; a rolling
    /// session always passes its ledger allocation.
    pub(crate) fn bind(dir: PathBuf, grants: Option<WriteGrants>) -> std::io::Result<PutSink> {
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let drain_listener = listener.try_clone()?;
        drain_listener.set_nonblocking(true)?;
        let addr = listener.local_addr()?;
        let listener = tokio::net::TcpListener::from_std(listener)?;
        let shared = Arc::new(Shared {
            dir,
            token: uuid::Uuid::new_v4().simple().to_string(),
            addr,
            drain_listener,
            grants,
            closed: AtomicBool::new(false),
            in_flight: AtomicUsize::new(0),
            idle: tokio::sync::Notify::new(),
            changed: tokio::sync::Notify::new(),
            commit: tokio::sync::Mutex::new(()),
            state: Mutex::new(State::default()),
            starved: OnceLock::new(),
            next_request: AtomicU64::new(0),
        });
        let (drains, requests) = tokio::sync::mpsc::unbounded_channel();
        let acceptor = tokio::spawn(accept_loop(Arc::clone(&shared), listener, requests));
        Ok(PutSink {
            shared,
            acceptor,
            drains,
        })
    }

    /// The output base FFmpeg writes one producer attempt's objects under.
    /// The HLS argument builders append `/<object>` to it.
    pub(crate) fn base_url(&self, lane: u32) -> String {
        format!("http://{}/{}/{lane}", self.shared.addr, self.shared.token)
    }

    /// Install the hook that asks the flow controller for an evaluation when
    /// a writer is refused a grant. Set once, after the session exists.
    pub(crate) fn on_starved(&self, hook: StarvedHook) {
        let _ = self.shared.starved.set(hook);
    }

    /// Wait until every upload the producer has already made is either in
    /// place or refused.
    ///
    /// Call this after the producer has exited. A loopback `connect` does
    /// not return until the kernel has queued the connection, so once the
    /// process is gone the set of connections is closed: this accepts every
    /// queued one directly, then waits for the requests to settle.
    pub(crate) async fn drain(&self) {
        let shared = &self.shared;
        let (swept, sweep) = tokio::sync::oneshot::channel();
        if self.drains.send(swept).is_ok() {
            // An error means the acceptor is gone, and with it anything it
            // could still have accepted.
            let _ = sweep.await;
        }
        loop {
            let settled = shared.idle.notified();
            if shared.in_flight.load(Ordering::Acquire) == 0 {
                return;
            }
            settled.await;
        }
    }

    #[cfg(test)]
    pub(crate) fn committed(&self, name: &str) -> bool {
        self.shared.lock().committed.contains(name)
    }
}

impl Drop for PutSink {
    fn drop(&mut self) {
        self.shared.closed.store(true, Ordering::Release);
        self.shared.changed.notify_waiters();
        // The acceptor only accepts; it never writes. Aborting it is safe.
        // Request tasks are deliberately not aborted: one may be inside a
        // file write, and a write abandoned mid-flight is not proof that it
        // stopped. They see `closed` and finish on their own.
        self.acceptor.abort();
    }
}

impl std::fmt::Debug for PutSink {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "PutSink({})", self.shared.addr)
    }
}

async fn accept_loop(
    shared: Arc<Shared>,
    listener: tokio::net::TcpListener,
    mut drains: tokio::sync::mpsc::UnboundedReceiver<tokio::sync::oneshot::Sender<()>>,
) {
    loop {
        tokio::select! {
            biased;
            request = drains.recv() => {
                let Some(swept) = request else {
                    return;
                };
                sweep(&shared);
                let _ = swept.send(());
            }
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _)) => shared.serve(stream),
                    Err(error) => {
                        tracing::warn!(%error, "scratch upload: accept failed");
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                }
            }
        }
    }
}

/// Accept every connection the kernel has already queued, without waiting
/// for readiness, and hand each one to its task.
fn sweep(shared: &Arc<Shared>) {
    loop {
        match shared.drain_listener.accept() {
            Ok((stream, _)) => {
                let adopted = stream
                    .set_nonblocking(true)
                    .and_then(|()| tokio::net::TcpStream::from_std(stream));
                match adopted {
                    Ok(stream) => shared.serve(stream),
                    Err(error) => {
                        tracing::warn!(%error, "scratch upload: could not adopt a queued connection");
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => {
                tracing::warn!(%error, "scratch upload: draining the listener failed");
                return;
            }
        }
    }
}

/// Why a request did not land.
#[derive(Debug)]
struct Refused {
    status: &'static str,
    reason: String,
}

impl Refused {
    fn new(status: &'static str, reason: impl Into<String>) -> Refused {
        Refused {
            status,
            reason: reason.into(),
        }
    }
}

async fn serve_connection(shared: &Arc<Shared>, stream: tokio::net::TcpStream) {
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::with_capacity(PIECE_BYTES, read);
    let status = match receive(shared, &mut reader).await {
        Ok(()) => "201 Created",
        Err(refused) => {
            tracing::debug!(reason = %refused.reason, "scratch upload refused");
            refused.status
        }
    };
    // FFmpeg does not read the reply. It is sent for anything else that
    // speaks to this socket, and so the connection closes cleanly.
    let _ = write
        .write_all(
            format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await;
    let _ = write.shutdown().await;
}

struct Request {
    lane: u32,
    name: String,
    body: BodyMode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BodyMode {
    Chunked,
    Length(u64),
}

async fn receive<R>(shared: &Arc<Shared>, reader: &mut R) -> Result<(), Refused>
where
    R: AsyncBufRead + Unpin,
{
    let request = read_request(shared, reader).await?;
    let playlist = request.name.ends_with(".m3u8");
    // Registration comes before the first byte and is held to the rename.
    // `None` means retirement fenced this allocation: its final inventory
    // may already be measured, and nothing may be added to it.
    let _writer: Option<ScratchWriter> = match shared.grants.as_ref() {
        Some(grants) => Some(
            grants
                .register_writer()
                .ok_or_else(|| Refused::new("410 Gone", "the session was retired"))?,
        ),
        None => None,
    };
    {
        let mut state = shared.lock();
        if request.lane < state.lane {
            return Err(Refused::new(
                "409 Conflict",
                format!("lane {} was replaced by lane {}", request.lane, state.lane),
            ));
        }
        if request.lane > state.lane {
            state.lane = request.lane;
            state.committed.clear();
            state.playlist_rank = None;
        }
    }
    let sequence = shared.next_request.fetch_add(1, Ordering::Relaxed);
    let temporary = shared.dir.join(format!("{}.{sequence}.tmp", request.name));
    let written = write_body(shared, reader, &request, &temporary, playlist).await;
    let copy = match written {
        Ok(written) => written,
        Err(refused) => {
            let _ = tokio::fs::remove_file(&temporary).await;
            return Err(refused);
        }
    };
    let committed = if playlist {
        commit_playlist(shared, &request, &temporary, &copy).await
    } else {
        commit_object(shared, &request, &temporary).await
    };
    if committed.is_err() {
        let _ = tokio::fs::remove_file(&temporary).await;
    }
    committed
}

async fn read_request<R>(shared: &Shared, reader: &mut R) -> Result<Request, Refused>
where
    R: AsyncBufRead + Unpin,
{
    let bad = |reason: &str| Refused::new("400 Bad Request", reason.to_owned());
    let mut head = 0_usize;
    let mut line = String::new();
    let request_line = read_line(reader, &mut line, &mut head).await?;
    let mut parts = request_line.split(' ');
    let method = parts.next().unwrap_or_default().to_owned();
    let target = parts.next().unwrap_or_default().to_owned();
    let mut chunked = false;
    let mut length = None;
    loop {
        let header = read_line(reader, &mut line, &mut head).await?;
        if header.is_empty() {
            break;
        }
        let Some((key, value)) = header.split_once(':') else {
            return Err(bad("malformed header"));
        };
        let value = value.trim();
        if key.eq_ignore_ascii_case("transfer-encoding") {
            chunked = value
                .split(',')
                .any(|coding| coding.trim().eq_ignore_ascii_case("chunked"));
        } else if key.eq_ignore_ascii_case("content-length") {
            length = Some(
                value
                    .parse::<u64>()
                    .map_err(|_| bad("bad content length"))?,
            );
        }
    }
    if method != "PUT" {
        return Err(Refused::new(
            "405 Method Not Allowed",
            format!("{method} is not an upload"),
        ));
    }
    let path = target.split('?').next().unwrap_or_default();
    let mut segments = path.strip_prefix('/').unwrap_or_default().split('/');
    let (Some(token), Some(lane), Some(name), None) = (
        segments.next(),
        segments.next(),
        segments.next(),
        segments.next(),
    ) else {
        return Err(not_found());
    };
    if !constant_time_eq(token.as_bytes(), shared.token.as_bytes()) {
        return Err(Refused::new("403 Forbidden", "wrong upload token"));
    }
    let lane = lane.parse::<u32>().map_err(|_| not_found())?;
    if !valid_object_name(name) {
        return Err(bad("not an HLS object name"));
    }
    let body = match (chunked, length) {
        (true, _) => BodyMode::Chunked,
        (false, Some(length)) => BodyMode::Length(length),
        (false, None) => return Err(Refused::new("411 Length Required", "no body framing")),
    };
    Ok(Request {
        lane,
        name: name.to_owned(),
        body,
    })
}

fn not_found() -> Refused {
    Refused::new("404 Not Found", "not an upload path")
}

async fn read_line<'a, R>(
    reader: &mut R,
    line: &'a mut String,
    head: &mut usize,
) -> Result<&'a str, Refused>
where
    R: AsyncBufRead + Unpin,
{
    line.clear();
    let read = reader
        .take((MAX_REQUEST_HEAD.saturating_sub(*head)) as u64)
        .read_line(line)
        .await
        .map_err(|error| {
            Refused::new("400 Bad Request", format!("reading the request: {error}"))
        })?;
    *head = head.saturating_add(read);
    if read == 0 || !line.ends_with('\n') {
        return Err(Refused::new(
            "400 Bad Request",
            "the request head ended early or is too large",
        ));
    }
    Ok(line.trim_end_matches(['\r', '\n']))
}

async fn write_body<R>(
    shared: &Arc<Shared>,
    reader: &mut R,
    request: &Request,
    temporary: &std::path::Path,
    playlist: bool,
) -> Result<Vec<u8>, Refused>
where
    R: AsyncBufRead + Unpin,
{
    let io = |what: &str, error: std::io::Error| {
        Refused::new("500 Internal Server Error", format!("{what}: {error}"))
    };
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temporary)
        .await
        .map_err(|error| io("creating the temporary object", error))?;
    let mut body = Body::new(request.body);
    let mut piece = vec![0_u8; PIECE_BYTES];
    let mut copy = Vec::new();
    let result = async {
        loop {
            let read = tokio::select! {
                read = body.read(reader, &mut piece) => read,
                () = closed(shared) => {
                    return Err(Refused::new("410 Gone", "the session ended"));
                }
            }
            .map_err(|error| {
                Refused::new("400 Bad Request", format!("reading the body: {error}"))
            })?;
            if read == 0 {
                break;
            }
            let read_bytes = i64::try_from(read).unwrap_or(i64::MAX);
            let authorized = match shared.grants.as_ref() {
                Some(grants) => Some(authorize(shared, grants, &request.name, read_bytes).await?),
                None => None,
            };
            let written = file.write_all(&piece[..read]).await;
            // Landed whether or not the write succeeded: a failed write can
            // still have put part of the piece on the disk, and the
            // temporary file stays charged as written until the caller
            // removes it and the next complete measurement subsumes it.
            if let Some(authorized) = authorized {
                authorized.landed(read_bytes);
            }
            written.map_err(|error| io("writing the temporary object", error))?;
            if playlist {
                if copy.len().saturating_add(read) > MAX_PLAYLIST_BYTES {
                    return Err(Refused::new(
                        "413 Payload Too Large",
                        "a playlist larger than any this daemon writes",
                    ));
                }
                copy.extend_from_slice(&piece[..read]);
            }
        }
        // A tokio file write returns before the bytes reach the file; the
        // flush is what makes the rename that follows publish them.
        file.flush()
            .await
            .map_err(|error| io("flushing the temporary object", error))?;
        Ok(())
    }
    .await;
    drop(file);
    result.map(|()| copy)
}

async fn closed(shared: &Shared) {
    loop {
        let changed = shared.changed.notified();
        if shared.is_closed() {
            return;
        }
        changed.await;
    }
}

/// Wait until the budget authorizes `bytes`, the session goes away, or a
/// session that has never published has waited long enough.
async fn authorize(
    shared: &Shared,
    grants: &WriteGrants,
    name: &str,
    bytes: i64,
) -> Result<ScratchWrite, Refused> {
    let started = tokio::time::Instant::now();
    let mut asked = false;
    loop {
        if shared.is_closed() {
            return Err(Refused::new("410 Gone", "the session ended"));
        }
        if grants.fenced() {
            return Err(Refused::new("410 Gone", "the session was retired"));
        }
        if let Some(write) = grants.authorize(usize::try_from(bytes).unwrap_or(usize::MAX)) {
            return Ok(write);
        }
        if !asked {
            asked = true;
            if let Some(hook) = shared.starved.get() {
                hook();
            }
        }
        let published = shared.lock().published;
        if !published && started.elapsed() >= PREPUBLICATION_GRANT_WAIT {
            let reason = format!(
                "rolling_insufficient_capacity: {name} needs {bytes} bytes before this session \
                 can publish anything, and the global budget did not free any in {}s",
                PREPUBLICATION_GRANT_WAIT.as_secs()
            );
            tracing::warn!("{reason}");
            return Err(Refused::new("507 Insufficient Storage", reason));
        }
        tokio::select! {
            () = tokio::time::sleep(GRANT_WAIT_POLL) => {}
            () = closed(shared) => {}
        }
    }
}

async fn commit_object(
    shared: &Arc<Shared>,
    request: &Request,
    temporary: &std::path::Path,
) -> Result<(), Refused> {
    let _commit = shared.commit.lock().await;
    check_current(shared, request.lane)?;
    tokio::fs::rename(temporary, shared.dir.join(&request.name))
        .await
        .map_err(|error| {
            Refused::new(
                "500 Internal Server Error",
                format!("publishing {}: {error}", request.name),
            )
        })?;
    shared.lock().committed.insert(request.name.clone());
    shared.changed.notify_waiters();
    Ok(())
}

async fn commit_playlist(
    shared: &Arc<Shared>,
    request: &Request,
    temporary: &std::path::Path,
    body: &[u8],
) -> Result<(), Refused> {
    let text = std::str::from_utf8(body)
        .map_err(|_| Refused::new("400 Bad Request", "the playlist is not UTF-8"))?;
    let named = playlist_references(text);
    let rank = playlist_rank(text);
    let deadline = tokio::time::Instant::now() + PLAYLIST_ORDER_WAIT;
    loop {
        let changed = shared.changed.notified();
        {
            let state = shared.lock();
            check_lane(shared, &state, request.lane)?;
            if named
                .iter()
                .all(|name| state.committed.contains(name.as_str()))
            {
                break;
            }
        }
        if tokio::time::timeout_at(deadline, changed).await.is_err() {
            let missing = {
                let state = shared.lock();
                named
                    .iter()
                    .filter(|name| !state.committed.contains(name.as_str()))
                    .cloned()
                    .collect::<Vec<_>>()
            };
            tracing::warn!(
                ?missing,
                "scratch upload: dropped a playlist version whose objects never landed"
            );
            return Err(Refused::new(
                "409 Conflict",
                "the playlist names objects that never landed",
            ));
        }
    }
    let _commit = shared.commit.lock().await;
    {
        let state = shared.lock();
        check_lane(shared, &state, request.lane)?;
        if state
            .playlist_rank
            .is_some_and(|committed| rank < committed)
        {
            // A newer version is already in place. This one is not an
            // error, it is simply late.
            return Err(Refused::new("200 OK", "superseded by a newer playlist"));
        }
    }
    tokio::fs::rename(temporary, shared.dir.join(&request.name))
        .await
        .map_err(|error| {
            Refused::new(
                "500 Internal Server Error",
                format!("publishing {}: {error}", request.name),
            )
        })?;
    {
        let mut state = shared.lock();
        state.committed.insert(request.name.clone());
        state.playlist_rank = Some(rank);
        state.published = true;
    }
    shared.changed.notify_waiters();
    Ok(())
}

fn check_current(shared: &Shared, lane: u32) -> Result<(), Refused> {
    let state = shared.lock();
    check_lane(shared, &state, lane)
}

fn check_lane(shared: &Shared, state: &State, lane: u32) -> Result<(), Refused> {
    if shared.is_closed() {
        return Err(Refused::new("410 Gone", "the session ended"));
    }
    if shared.grants.as_ref().is_some_and(WriteGrants::fenced) {
        return Err(Refused::new("410 Gone", "the session was retired"));
    }
    if state.lane != lane {
        return Err(Refused::new(
            "409 Conflict",
            format!("lane {lane} was replaced by lane {}", state.lane),
        ));
    }
    Ok(())
}

/// Every object a playlist names: its media URIs and its `EXT-X-MAP` init.
fn playlist_references(text: &str) -> Vec<String> {
    let mut named = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(tag) = line.strip_prefix("#EXT-X-MAP:") {
            if let Some(uri) = attribute(tag, "URI") {
                named.push(object_of(uri));
            }
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        named.push(object_of(line));
    }
    named
}

fn playlist_rank(text: &str) -> (usize, bool) {
    let entries = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .count();
    (entries, text.contains("#EXT-X-ENDLIST"))
}

fn attribute<'a>(tag: &'a str, key: &str) -> Option<&'a str> {
    let start = tag.find(&format!("{key}=\""))? + key.len() + 2;
    let rest = &tag[start..];
    rest.find('"').map(|end| &rest[..end])
}

fn object_of(uri: &str) -> String {
    let path = uri.split(['?', '#']).next().unwrap_or_default();
    path.rsplit('/').next().unwrap_or_default().to_owned()
}

/// The names FFmpeg's HLS muxer produces for a rolling session, and nothing
/// that could step outside the directory or collide with a temporary.
fn valid_object_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        && [".ts", ".m4s", ".mp4", ".m3u8", ".vtt"]
            .iter()
            .any(|suffix| name.ends_with(suffix))
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
            == 0
}

/// Replace every upload token in a line of text, so argv and FFmpeg's own
/// log lines can be logged without handing out a write capability.
pub(crate) fn redact(text: &str) -> String {
    const MARKER: &str = "http://127.0.0.1:";
    if !text.contains(MARKER) {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(MARKER) {
        let after = at + MARKER.len();
        out.push_str(&rest[..after]);
        rest = &rest[after..];
        let port_end = rest.find('/').unwrap_or(rest.len());
        out.push_str(&rest[..port_end]);
        rest = &rest[port_end..];
        if let Some(path) = rest.strip_prefix('/') {
            let token_end = path.find('/').unwrap_or(path.len());
            let token = &path[..token_end];
            if token.len() == 32 && token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                out.push_str("/REDACTED");
                rest = &path[token_end..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// A request body, chunked or length-delimited, read one piece at a time so
/// that nothing is read from the socket before there is somewhere to put it.
struct Body {
    mode: BodyMode,
    /// Bytes left in the current chunk, or in the whole body.
    remaining: u64,
    started: bool,
    done: bool,
}

impl Body {
    fn new(mode: BodyMode) -> Body {
        Body {
            mode,
            remaining: match mode {
                BodyMode::Length(length) => length,
                BodyMode::Chunked => 0,
            },
            started: false,
            done: false,
        }
    }

    async fn read<R>(&mut self, reader: &mut R, piece: &mut [u8]) -> std::io::Result<usize>
    where
        R: AsyncBufRead + Unpin,
    {
        if self.done {
            return Ok(0);
        }
        if self.remaining == 0 {
            match self.mode {
                BodyMode::Length(_) => {
                    self.done = true;
                    return Ok(0);
                }
                BodyMode::Chunked => {
                    if self.started {
                        expect_crlf(reader).await?;
                    }
                    self.started = true;
                    let size = read_chunk_size(reader).await?;
                    if size == 0 {
                        skip_trailers(reader).await?;
                        self.done = true;
                        return Ok(0);
                    }
                    self.remaining = size;
                }
            }
        }
        let wanted = usize::try_from(self.remaining.min(piece.len() as u64)).unwrap_or(piece.len());
        let read = reader.read(&mut piece[..wanted]).await?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "the upload ended before its body did",
            ));
        }
        self.remaining -= read as u64;
        Ok(read)
    }
}

async fn read_small_line<R>(reader: &mut R) -> std::io::Result<String>
where
    R: AsyncBufRead + Unpin,
{
    let mut line = String::new();
    let read = reader.take(1024).read_line(&mut line).await?;
    if read == 0 || !line.ends_with('\n') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "a chunk line ended early or is too long",
        ));
    }
    Ok(line)
}

async fn expect_crlf<R>(reader: &mut R) -> std::io::Result<()>
where
    R: AsyncBufRead + Unpin,
{
    let line = read_small_line(reader).await?;
    if line.trim_end_matches(['\r', '\n']).is_empty() {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "a chunk was longer than its declared size",
        ))
    }
}

async fn read_chunk_size<R>(reader: &mut R) -> std::io::Result<u64>
where
    R: AsyncBufRead + Unpin,
{
    let line = read_small_line(reader).await?;
    let size = line
        .trim_end_matches(['\r', '\n'])
        .split(';')
        .next()
        .unwrap_or_default()
        .trim();
    u64::from_str_radix(size, 16).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "a chunk size is not hex")
    })
}

async fn skip_trailers<R>(reader: &mut R) -> std::io::Result<()>
where
    R: AsyncBufRead + Unpin,
{
    loop {
        let line = read_small_line(reader).await?;
        if line.trim_end_matches(['\r', '\n']).is_empty() {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch_ledger::ScratchLedger;
    use std::sync::atomic::AtomicI64;

    const MIB: i64 = 1 << 20;

    struct Fixture {
        _root: tempfile::TempDir,
        dir: PathBuf,
        ledger: Arc<ScratchLedger>,
        permit: crate::scratch_ledger::ScratchPermit,
        configured: Arc<AtomicI64>,
        sink: PutSink,
    }

    fn fixture(grant: i64, configured: i64) -> Fixture {
        let root = tempfile::tempdir().expect("tempdir");
        let dir = root.path().join("w");
        std::fs::create_dir_all(&dir).expect("session dir");
        let ledger = ScratchLedger::new();
        let permit = ledger.reserve(grant, configured).expect("admission");
        let configured = Arc::new(AtomicI64::new(configured));
        let grants = WriteGrants::new(
            Arc::clone(&ledger),
            permit.key(),
            Arc::clone(&configured),
            0,
        );
        let sink = PutSink::bind(dir.clone(), Some(grants)).expect("bind");
        Fixture {
            _root: root,
            dir,
            ledger,
            permit,
            configured,
            sink,
        }
    }

    fn host_path(base: &str) -> (String, String) {
        let rest = base.strip_prefix("http://").expect("http base");
        let (host, path) = rest.split_once('/').expect("path");
        (host.to_owned(), format!("/{path}"))
    }

    async fn open_put(base: &str, name: &str, length: Option<usize>) -> tokio::net::TcpStream {
        let (host, path) = host_path(base);
        let mut stream = tokio::net::TcpStream::connect(&host)
            .await
            .expect("connect");
        let framing = match length {
            Some(length) => format!("Content-Length: {length}\r\n"),
            None => "Transfer-Encoding: chunked\r\n".to_owned(),
        };
        stream
            .write_all(
                format!("PUT {path}/{name} HTTP/1.1\r\nHost: {host}\r\n{framing}\r\n").as_bytes(),
            )
            .await
            .expect("head");
        stream
    }

    /// Write errors are ignored: the receiver refuses some requests as
    /// soon as it has read their head, and a client still writing the body
    /// then sees a reset. What each test asserts is the reply and the
    /// directory, never whether a write raced a refusal.
    async fn chunk(stream: &mut tokio::net::TcpStream, bytes: &[u8]) {
        let _ = stream
            .write_all(format!("{:x}\r\n", bytes.len()).as_bytes())
            .await;
        let _ = stream.write_all(bytes).await;
        let _ = stream.write_all(b"\r\n").await;
    }

    async fn finish(mut stream: tokio::net::TcpStream) -> String {
        // The receiver may already have refused and closed; the reply is
        // what the assertion is about, not whether the last chunk went out.
        let _ = stream.write_all(b"0\r\n\r\n").await;
        let mut reply = String::new();
        let _ = stream.read_to_string(&mut reply).await;
        reply
    }

    async fn put(base: &str, name: &str, bytes: &[u8]) -> String {
        let mut stream = open_put(base, name, None).await;
        chunk(&mut stream, bytes).await;
        finish(stream).await
    }

    fn regular_bytes(dir: &std::path::Path) -> u64 {
        std::fs::read_dir(dir)
            .expect("list")
            .filter_map(Result::ok)
            .filter_map(|entry| entry.metadata().ok())
            .filter(std::fs::Metadata::is_file)
            .map(|metadata| metadata.len())
            .sum()
    }

    async fn eventually(mut condition: impl FnMut() -> bool) -> bool {
        for _ in 0..200 {
            if condition() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        condition()
    }

    /// The claim this whole module rests on: routing FFmpeg's HLS muxer
    /// through the receiver changes where its bytes go and nothing about
    /// what they are. The same encode, once to files and once through
    /// `-method PUT`, produces the same objects, byte for byte.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn scratch_put_ffmpeg_output_is_byte_identical_to_file_output() {
        let fixture = fixture(64 << 20, 64 << 20);
        let files = fixture._root.path().join("file");
        std::fs::create_dir_all(&files).expect("file dir");
        let encode = |segment: String, playlist: String, put: bool| {
            let mut args: Vec<String> = [
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostdin",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=320x180:rate=24",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440",
                "-t",
                "7",
                "-c:v",
                "libx264",
                "-preset",
                "veryfast",
                "-threads",
                "1",
                "-force_key_frames",
                "expr:gte(t,n_forced*2)",
                "-c:a",
                "aac",
                "-muxdelay",
                "0",
                "-muxpreload",
                "0",
                "-f",
                "hls",
                "-hls_time",
                "2",
                "-hls_playlist_type",
                "event",
                "-hls_segment_type",
                "mpegts",
            ]
            .map(str::to_owned)
            .to_vec();
            if put {
                args.extend(
                    ["-hls_flags", "independent_segments", "-method", "PUT"].map(str::to_owned),
                );
            } else {
                args.extend(["-hls_flags", "independent_segments+temp_file"].map(str::to_owned));
            }
            args.extend(["-hls_segment_filename".to_owned(), segment, playlist]);
            let status = std::process::Command::new(plurx_core::testfixtures::ffmpeg())
                .args(&args)
                .status()
                .expect("ffmpeg runs");
            assert!(status.success(), "ffmpeg failed: {args:?}");
        };
        let base = fixture.sink.base_url(0);
        let put_segment = format!("{base}/seg%05d.ts");
        let put_playlist = format!("{base}/index.m3u8");
        tokio::task::spawn_blocking(move || encode(put_segment, put_playlist, true))
            .await
            .expect("put encode");
        fixture.sink.drain().await;
        let file_segment = files.join("seg%05d.ts").to_string_lossy().into_owned();
        let file_playlist = files.join("index.m3u8").to_string_lossy().into_owned();
        tokio::task::spawn_blocking(move || encode(file_segment, file_playlist, false))
            .await
            .expect("file encode");

        let mut expected = std::fs::read_dir(&files)
            .expect("list")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .into_string()
                    .expect("utf8")
            })
            .collect::<Vec<_>>();
        expected.sort();
        let mut landed = std::fs::read_dir(&fixture.dir)
            .expect("list")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .into_string()
                    .expect("utf8")
            })
            .collect::<Vec<_>>();
        landed.sort();
        assert_eq!(
            landed, expected,
            "the same objects, and no temporaries left behind"
        );
        assert!(
            expected.len() >= 4,
            "a playlist and several segments: {expected:?}"
        );
        for name in &expected {
            assert_eq!(
                std::fs::read(fixture.dir.join(name)).expect("put object"),
                std::fs::read(files.join(name)).expect("file object"),
                "{name} differs between file and PUT output"
            );
        }
        assert!(
            std::fs::read_to_string(fixture.dir.join("index.m3u8"))
                .expect("playlist")
                .contains("#EXT-X-ENDLIST"),
            "the final playlist landed before drain returned"
        );
        drop(fixture.permit);
    }

    /// A refused grant is backpressure, and it is exact: nothing beyond the
    /// authorized bytes reaches the directory while the budget says no, and
    /// the same upload completes once the budget allows it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn scratch_put_a_refused_grant_writes_nothing_past_the_allowance() {
        let allowance = 2 * MIB;
        let fixture = fixture(allowance, allowance);
        let base = fixture.sink.base_url(0);
        let body = vec![7_u8; 5 << 20];
        let sender = {
            let body = body.clone();
            tokio::spawn(async move {
                let mut stream = open_put(&base, "seg00000.ts", Some(body.len())).await;
                stream.write_all(&body).await.expect("body");
                let mut reply = String::new();
                let _ = stream.read_to_string(&mut reply).await;
                reply
            })
        };
        tokio::time::sleep(Duration::from_millis(750)).await;
        let on_disk = regular_bytes(&fixture.dir);
        assert!(
            on_disk <= allowance as u64,
            "{on_disk} bytes on disk against a {allowance}-byte allowance"
        );
        assert!(!fixture.sink.committed("seg00000.ts"));
        assert!(
            fixture.ledger.charge_of(fixture.permit.key()) <= Some(allowance),
            "the charge never exceeds what the budget admitted"
        );

        fixture.configured.store(64 << 20, Ordering::Relaxed);
        let reply = tokio::time::timeout(Duration::from_secs(10), sender)
            .await
            .expect("the upload completes once the budget grows")
            .expect("sender");
        assert!(reply.starts_with("HTTP/1.1 201"), "{reply}");
        assert_eq!(
            std::fs::read(fixture.dir.join("seg00000.ts")).expect("landed"),
            body
        );
        drop(fixture.permit);
    }

    /// FFmpeg names a segment in its playlist as soon as it has *sent* the
    /// segment, not when a receiver has finished writing it. File output
    /// never exposed a playlist that named a missing object; neither may
    /// this.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn scratch_put_a_playlist_waits_for_the_objects_it_names() {
        let fixture = fixture(64 << 20, 64 << 20);
        let base = fixture.sink.base_url(0);
        let mut segment = open_put(&base, "seg00000.ts", None).await;
        chunk(&mut segment, &[1_u8; 4096]).await;
        let playlist = {
            let base = base.clone();
            tokio::spawn(async move {
                put(
                    &base,
                    "index.m3u8",
                    b"#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2.0,\nseg00000.ts\n",
                )
                .await
            })
        };
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            !fixture.dir.join("index.m3u8").exists(),
            "the playlist cannot land before the segment it names"
        );
        chunk(&mut segment, &[2_u8; 4096]).await;
        assert!(finish(segment).await.starts_with("HTTP/1.1 201"));
        let reply = playlist.await.expect("playlist");
        assert!(reply.starts_with("HTTP/1.1 201"), "{reply}");
        assert!(fixture.dir.join("index.m3u8").exists());

        // And a stale version never replaces a newer one.
        let newer = "#EXTM3U\n#EXTINF:2.0,\nseg00000.ts\n#EXT-X-ENDLIST\n";
        assert!(put(&base, "index.m3u8", newer.as_bytes())
            .await
            .starts_with("HTTP/1.1 201"));
        let _ = put(&base, "index.m3u8", b"#EXTM3U\n").await;
        assert_eq!(
            std::fs::read_to_string(fixture.dir.join("index.m3u8")).expect("playlist"),
            newer
        );
        drop(fixture.permit);
    }

    /// A replaced producer attempt keeps uploading until it is reaped. Its
    /// late objects must never land over its successor's.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn scratch_put_a_replaced_lane_cannot_overwrite_its_successor() {
        let fixture = fixture(64 << 20, 64 << 20);
        let old = fixture.sink.base_url(0);
        let new = fixture.sink.base_url(1);
        let mut late = open_put(&old, "seg00000.ts", None).await;
        chunk(&mut late, b"old").await;
        assert!(put(&new, "seg00000.ts", b"new")
            .await
            .starts_with("HTTP/1.1 201"));
        assert!(finish(late).await.starts_with("HTTP/1.1 409"));
        assert!(put(&old, "seg00001.ts", b"old")
            .await
            .starts_with("HTTP/1.1 409"));
        assert_eq!(
            std::fs::read(fixture.dir.join("seg00000.ts")).expect("landed"),
            b"new"
        );
        assert!(!fixture.dir.join("seg00001.ts").exists());
        assert!(
            eventually(|| regular_bytes(&fixture.dir) == 3).await,
            "the refused uploads left no temporary behind"
        );
        drop(fixture.permit);
    }

    /// Every upload holds a writer registration, so retirement's writer
    /// barrier waits for it, and the fence wakes a writer that is waiting
    /// for a grant instead of leaving the barrier to time out.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn scratch_put_an_upload_is_a_scratch_writer_until_it_settles() {
        let fixture = fixture(MIB, MIB);
        let key = fixture.permit.key();
        let base = fixture.sink.base_url(0);
        let mut stream = open_put(&base, "seg00000.ts", None).await;
        chunk(&mut stream, &vec![3_u8; MIB as usize]).await;
        chunk(&mut stream, &[3_u8; 4096]).await;
        assert!(
            eventually(|| !fixture.ledger.writers_settled(key)).await,
            "the upload registered as a writer"
        );
        let barrier = fixture.ledger.begin_retirement(key).expect("entry");
        assert!(
            barrier
                .settle(
                    &fixture.ledger,
                    key,
                    tokio::time::Instant::now() + Duration::from_secs(5)
                )
                .await,
            "the fence woke the starved upload and it settled"
        );
        let reply = finish(stream).await;
        assert!(
            reply.is_empty() || reply.starts_with("HTTP/1.1 410"),
            "{reply}"
        );
        assert!(!fixture.dir.join("seg00000.ts").exists());
        assert!(put(&base, "seg00001.ts", b"x")
            .await
            .starts_with("HTTP/1.1 410"));
        drop(fixture.permit);
    }

    /// After the producer is gone, `drain` is the moment its uploads are all
    /// in place: it adopts connections still queued on the socket.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn scratch_put_drain_covers_an_upload_still_queued_on_the_socket() {
        let fixture = fixture(64 << 20, 64 << 20);
        let base = fixture.sink.base_url(0);
        let mut stream = open_put(&base, "seg00007.ts", None).await;
        chunk(&mut stream, b"tail").await;
        stream.write_all(b"0\r\n\r\n").await.expect("last chunk");
        drop(stream);
        fixture.sink.drain().await;
        assert!(fixture.sink.committed("seg00007.ts"));
        assert_eq!(
            std::fs::read(fixture.dir.join("seg00007.ts")).expect("landed"),
            b"tail"
        );
        drop(fixture.permit);
    }

    #[tokio::test]
    async fn scratch_put_refuses_anything_that_is_not_an_hls_upload() {
        let fixture = fixture(64 << 20, 64 << 20);
        let base = fixture.sink.base_url(0);
        let (host, path) = host_path(&base);
        let wrong_token = format!("http://{host}/{}/0", "0".repeat(32));
        assert!(put(&wrong_token, "seg00000.ts", b"x")
            .await
            .starts_with("HTTP/1.1 403"));
        assert!(put(&base, "..", b"x").await.starts_with("HTTP/1.1 400"));
        assert!(put(&base, "notes.txt", b"x")
            .await
            .starts_with("HTTP/1.1 400"));
        assert!(put(&base, "seg.ts.tmp", b"x")
            .await
            .starts_with("HTTP/1.1 400"));
        let mut get = tokio::net::TcpStream::connect(&host)
            .await
            .expect("connect");
        get.write_all(format!("GET {path}/index.m3u8 HTTP/1.1\r\n\r\n").as_bytes())
            .await
            .expect("get");
        let mut reply = String::new();
        let _ = get.read_to_string(&mut reply).await;
        assert!(reply.starts_with("HTTP/1.1 405"), "{reply}");
        assert_eq!(regular_bytes(&fixture.dir), 0);
        drop(fixture.permit);
    }

    #[test]
    fn scratch_put_logs_never_carry_the_upload_token() {
        let token = "0123456789abcdef0123456789abcdef";
        let line = format!(
            "-hls_segment_filename http://127.0.0.1:41234/{token}/0/seg%05d.ts \
             http://127.0.0.1:41234/{token}/0/index.m3u8"
        );
        let redacted = redact(&line);
        assert!(!redacted.contains(token), "{redacted}");
        assert!(redacted.contains("http://127.0.0.1:41234/REDACTED/0/index.m3u8"));
        assert_eq!(redact("no urls here"), "no urls here");
    }

    #[test]
    fn scratch_put_playlist_references_include_the_init_map() {
        let text = "#EXTM3U\n#EXT-X-MAP:URI=\"init-e3.mp4\"\n#EXTINF:4.0,\nseg00003.m4s\n";
        assert_eq!(
            playlist_references(text),
            vec!["init-e3.mp4".to_owned(), "seg00003.m4s".to_owned()]
        );
        assert_eq!(playlist_rank(text), (1, false));
    }

    /// The copy path's own FFmpeg muxer -- the legacy writer, a takeover,
    /// and the retry a segmenter session keeps in reserve -- writes fMP4 with
    /// an init segment the playlist names in `EXT-X-MAP`. Through the
    /// receiver it produces the same objects as it does to files.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn scratch_put_fmp4_copy_output_is_byte_identical_to_file_output() {
        let fixture = fixture(64 << 20, 64 << 20);
        let files = fixture._root.path().join("file");
        std::fs::create_dir_all(&files).expect("file dir");
        let source = plurx_core::testfixtures::source("h264");
        let remux = move |base: String, put: bool| {
            let mut args: Vec<String> = vec![
                "-hide_banner".into(),
                "-loglevel".into(),
                "error".into(),
                "-nostdin".into(),
                "-i".into(),
                source.to_string_lossy().into_owned(),
                "-map".into(),
                "0:v:0".into(),
                "-map".into(),
                "0:a:0".into(),
                "-c".into(),
                "copy".into(),
                "-f".into(),
                "hls".into(),
                "-hls_time".into(),
                "4".into(),
                "-hls_playlist_type".into(),
                "event".into(),
            ];
            if put {
                args.extend(["-method", "PUT"].map(str::to_owned));
            } else {
                args.extend(["-hls_flags", "temp_file"].map(str::to_owned));
            }
            args.extend(
                [
                    "-hls_segment_type",
                    "fmp4",
                    "-hls_fmp4_init_filename",
                    "init-e3.mp4",
                ]
                .map(str::to_owned),
            );
            args.push("-hls_segment_filename".into());
            args.push(format!("{base}/seg%05d.m4s"));
            args.push(format!("{base}/index.m3u8"));
            let status = std::process::Command::new(plurx_core::testfixtures::ffmpeg())
                .args(&args)
                .status()
                .expect("ffmpeg runs");
            assert!(status.success(), "ffmpeg failed: {args:?}");
        };
        let base = fixture.sink.base_url(3);
        let file_base = files.to_string_lossy().into_owned();
        let put_remux = remux.clone();
        tokio::task::spawn_blocking(move || put_remux(base, true))
            .await
            .expect("put remux");
        fixture.sink.drain().await;
        tokio::task::spawn_blocking(move || remux(file_base, false))
            .await
            .expect("file remux");
        let names = |dir: &std::path::Path| {
            let mut names = std::fs::read_dir(dir)
                .expect("list")
                .map(|entry| {
                    entry
                        .expect("entry")
                        .file_name()
                        .into_string()
                        .expect("utf8")
                })
                .collect::<Vec<_>>();
            names.sort();
            names
        };
        let expected = names(&files);
        assert_eq!(names(&fixture.dir), expected);
        assert!(
            expected.iter().any(|name| name == "init-e3.mp4"),
            "{expected:?}"
        );
        for name in &expected {
            assert_eq!(
                std::fs::read(fixture.dir.join(name)).expect("put object"),
                std::fs::read(files.join(name)).expect("file object"),
                "{name} differs between file and PUT output"
            );
        }
        drop(fixture.permit);
    }
}
