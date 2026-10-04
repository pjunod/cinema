//! The Source direct byte exchange on B: one fixed POST per viewer request,
//! a head verified against B's own Local plan, and a body bounded by an idle
//! deadline rather than a whole-file deadline.
use super::*;
use crate::http::sharing_direct_wire::{DirectByteRequest, SourceDirectHead, SourceDirectStart};

/// How long an open direct body may receive nothing from the Source. A film
/// takes far longer than any fixed total, so only silence ends a body.
pub(crate) const DIRECT_BODY_IDLE: Duration = Duration::from_secs(30);
/// Connect-to-head bound for one direct exchange (the resource RPC's).
const DIRECT_HEAD_DEADLINE: Duration = Duration::from_secs(30);
/// Hyper may coalesce several Source writes; B hands the viewer at most this.
const DIRECT_CHUNK: usize = 64 * 1024;

/// A verified Source direct answer: the exact Local status/header set for
/// this request and a body that retains the actual upstream connection.
pub(crate) struct SourcePeerDirect {
    pub(crate) head: SourceDirectHead,
    pub(crate) body: Body,
}

impl PeerConnection {
    pub(crate) async fn file_direct(
        mut self,
        credential: &Secret,
        viewer_hash: &str,
        session: &SourcePeerSession,
        known: &SourcePeerLineage,
        demand: &DirectByteRequest,
        expected: &SourceDirectStart,
    ) -> Result<SourcePeerDirect, PeerError> {
        if self.verified_endpoint.is_none()
            || !session.is_direct()
            || viewer_hash.len() != 64
            || !viewer_hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(PeerError::InvalidResponse);
        }
        demand.headers().map_err(|_| PeerError::InvalidResponse)?;
        let mut value: Value = serde_json::from_slice(&session.end_body(Some(known))?)
            .map_err(|_| PeerError::InvalidResponse)?;
        value["direct"] = serde_json::to_value(demand).map_err(|_| PeerError::InvalidResponse)?;
        let body = serde_json::to_vec(&value).map_err(|_| PeerError::InvalidResponse)?;
        if body.len() > 128 * 1024 {
            return Err(PeerError::InvalidResponse);
        }
        let response = tokio::time::timeout(DIRECT_HEAD_DEADLINE, async {
            let mut auth = HeaderValue::from_str(&format!("CinemaShare {}", credential.expose()))
                .map_err(|_| PeerError::InvalidResponse)?;
            auth.set_sensitive(true);
            let mut viewer =
                HeaderValue::from_str(viewer_hash).map_err(|_| PeerError::InvalidResponse)?;
            viewer.set_sensitive(true);
            let request = Request::builder()
                .method(Method::POST)
                .uri(format!(
                    "/sharing/v1/items/{}/files/{}/sessions/{}/direct",
                    session.reference.item_id.as_str(),
                    session.reference.file_id.as_str(),
                    session.request_id
                ))
                .header(header::HOST, &self.host)
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, auth)
                .header("cinemashare-viewer", viewer)
                .body(Body::from(body))
                .map_err(|_| PeerError::InvalidResponse)?;
            self.sender
                .ready()
                .await
                .map_err(|_| PeerError::Unavailable)?;
            self.sender
                .send_request(request)
                .await
                .map_err(|_| PeerError::Unavailable)
        })
        .await
        .map_err(|_| PeerError::Unavailable)??;
        let status = response.status();
        match status {
            StatusCode::OK | StatusCode::PARTIAL_CONTENT | StatusCode::RANGE_NOT_SATISFIABLE => {}
            StatusCode::UNAUTHORIZED => return Err(PeerError::Authentication),
            StatusCode::UPGRADE_REQUIRED => return Err(PeerError::ProtocolUnsupported),
            status => return Err(PeerError::Rejected(status)),
        }
        let echo = [
            (
                "cinemashare-reference",
                serde_json::to_string(&session.reference)
                    .map_err(|_| PeerError::InvalidResponse)?,
            ),
            ("cinemashare-request-id", session.request_id.to_string()),
            (
                "cinemashare-incarnation-id",
                known.incarnation_id.to_string(),
            ),
            ("cinemashare-session-id", known.session_id.to_string()),
            ("cinemashare-control-epoch", known.control_epoch.to_string()),
        ];
        let head = SourceDirectHead::parse(
            status,
            response.headers(),
            &echo,
            demand,
            expected.length,
            &expected.mime,
        )
        .map_err(|_| PeerError::InvalidResponse)?;
        let body = if head.body_length == 0 {
            let bytes = tokio::time::timeout(
                DIRECT_BODY_IDLE,
                axum::body::to_bytes(Body::new(response.into_body()), 0),
            )
            .await
            .map_err(|_| PeerError::Unavailable)?
            .map_err(|_| PeerError::InvalidResponse)?;
            if !bytes.is_empty() {
                return Err(PeerError::InvalidResponse);
            }
            // Close and join the actual upstream socket before answering.
            self.connection.abort();
            let _ = (&mut self.connection).await;
            Body::empty()
        } else {
            bounded_direct_body(
                self,
                Body::new(response.into_body()),
                head.body_length,
                DIRECT_BODY_IDLE,
            )
        };
        Ok(SourcePeerDirect { head, body })
    }
}

struct DirectStreamState<O> {
    _owner: O,
    incoming: Body,
    remaining: u64,
    pending: axum::body::Bytes,
    idle: Duration,
}
/// Relay exactly `remaining` bytes. Each poll pulls at most one upstream
/// frame and only when the previous one has been handed on, so application
/// buffering is one transport frame; Hyper's own read buffer bounds that.
/// The retained owner (the upstream connection) drops with the stream.
fn bounded_direct_body<O: Send + 'static>(
    owner: O,
    incoming: Body,
    remaining: u64,
    idle: Duration,
) -> Body {
    let stream = futures_util::stream::unfold(
        Some(DirectStreamState {
            _owner: owner,
            incoming,
            remaining,
            pending: axum::body::Bytes::new(),
            idle,
        }),
        |state| async move {
            let mut state = state?;
            loop {
                if !state.pending.is_empty() {
                    let bytes = state
                        .pending
                        .split_to(state.pending.len().min(DIRECT_CHUNK));
                    return Some((Ok(bytes), Some(state)));
                }
                match tokio::time::timeout(state.idle, state.incoming.frame()).await {
                    Ok(Some(Ok(frame))) => {
                        let Ok(bytes) = frame.into_data() else {
                            return Some((
                                Err(std::io::Error::other("sharing direct trailers")),
                                None,
                            ));
                        };
                        if bytes.len() as u64 > state.remaining {
                            return Some((
                                Err(std::io::Error::other("sharing direct length")),
                                None,
                            ));
                        }
                        state.remaining -= bytes.len() as u64;
                        state.pending = bytes;
                    }
                    Ok(None) if state.remaining == 0 => return None,
                    Err(_) => {
                        return Some((Err(std::io::Error::other("sharing direct body idle")), None))
                    }
                    _ => {
                        return Some((
                            Err(std::io::Error::other("sharing direct body incomplete")),
                            None,
                        ))
                    }
                }
            }
        },
    );
    Body::from_stream(stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    #[tokio::test(start_paused = true)]
    async fn sharing_receiver_direct_body_outlives_resource_deadline() {
        // Six chunks 20 s apart: 100 s of body, past the resource RPC's fixed
        // 30 s total, is fine while the Source keeps sending.
        let paced = futures_util::stream::unfold(0_u32, |sent| async move {
            if sent == 6 {
                return None;
            }
            if sent > 0 {
                tokio::time::sleep(Duration::from_secs(20)).await;
            }
            Some((
                Ok::<_, std::io::Error>(axum::body::Bytes::from_static(b"0123456789")),
                sent + 1,
            ))
        });
        let started = tokio::time::Instant::now();
        let body = bounded_direct_body((), Body::from_stream(paced), 60, DIRECT_BODY_IDLE);
        let bytes = axum::body::to_bytes(body, 1024).await.expect("paced body");
        assert_eq!(bytes.len(), 60);
        assert!(started.elapsed() >= Duration::from_secs(100));
        // Silence past the idle bound ends the body as an error, never EOF.
        let silent = futures_util::stream::unfold(0_u32, |sent| async move {
            if sent == 1 {
                tokio::time::sleep(DIRECT_BODY_IDLE + Duration::from_secs(1)).await;
            }
            Some((
                Ok::<_, std::io::Error>(axum::body::Bytes::from_static(b"01234")),
                sent + 1,
            ))
        });
        let body = bounded_direct_body((), Body::from_stream(silent), 10, DIRECT_BODY_IDLE);
        assert!(axum::body::to_bytes(body, 1024).await.is_err());
        // A short or overlong upstream is never a complete body.
        let short = futures_util::stream::iter([Ok::<_, std::io::Error>(
            axum::body::Bytes::from_static(b"0123"),
        )]);
        let body = bounded_direct_body((), Body::from_stream(short), 10, DIRECT_BODY_IDLE);
        assert!(axum::body::to_bytes(body, 1024).await.is_err());
        let long = futures_util::stream::iter([Ok::<_, std::io::Error>(
            axum::body::Bytes::from_static(b"0123456789AB"),
        )]);
        let body = bounded_direct_body((), Body::from_stream(long), 10, DIRECT_BODY_IDLE);
        assert!(axum::body::to_bytes(body, 1024).await.is_err());
    }

    #[tokio::test]
    async fn sharing_receiver_direct_slow_reader_memory_bounded() {
        use futures_util::StreamExt;
        let pulled = Arc::new(AtomicUsize::new(0));
        let counter = pulled.clone();
        let frame = axum::body::Bytes::from(vec![7_u8; 256 * 1024]);
        let upstream = futures_util::stream::unfold(0_usize, move |sent| {
            let counter = counter.clone();
            let frame = frame.clone();
            async move {
                (sent < 4).then(|| {
                    counter.fetch_add(1, Ordering::SeqCst);
                    (Ok::<_, std::io::Error>(frame), sent + 1)
                })
            }
        });
        let owner = Arc::new(());
        let body = bounded_direct_body(
            owner.clone(),
            Body::from_stream(upstream),
            1024 * 1024,
            DIRECT_BODY_IDLE,
        );
        let mut stream = body.into_data_stream();
        let first = stream.next().await.expect("chunk").expect("bytes");
        assert_eq!(first.len(), DIRECT_CHUNK);
        // A reader that has taken one chunk has caused exactly one upstream
        // frame to be buffered; nothing reads ahead of the viewer.
        assert_eq!(pulled.load(Ordering::SeqCst), 1);
        // The retained upstream owner lives exactly as long as the body.
        assert_eq!(Arc::strong_count(&owner), 2);
        for _ in 1..4 {
            assert_eq!(
                stream.next().await.expect("chunk").expect("bytes").len(),
                DIRECT_CHUNK
            );
        }
        assert_eq!(pulled.load(Ordering::SeqCst), 1);
        let fifth = stream.next().await.expect("chunk").expect("bytes");
        assert_eq!(fifth.len(), DIRECT_CHUNK);
        assert_eq!(pulled.load(Ordering::SeqCst), 2);
        let mut total = 5 * DIRECT_CHUNK;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.expect("bytes");
            assert!(chunk.len() <= DIRECT_CHUNK);
            total += chunk.len();
        }
        assert_eq!(total, 1024 * 1024);
        drop(stream);
        assert_eq!(Arc::strong_count(&owner), 1);
    }
}
