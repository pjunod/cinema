//! One closed pre-session file asset read from the pinned Source. The request
//! names the file-relative resource in its path and the complete expected
//! Source reference in one header; the reply is a complete bounded body whose
//! type, length and leading bytes match the resource kind, or a manifest that
//! is still being prepared.
use super::*;
use crate::http::{
    hls::SourcePlaybackTarget,
    shared_receiver_assets::{MAX_IMAGE_BYTES, MAX_MANIFEST_BYTES, MAX_VTT_BYTES},
    shared_source_assets::{encode_reference, REFERENCE_HEADER},
};
use plurx_core::sharing_resources::{SharingFileResource, SharingFileResourceKind as Kind};

/// The whole Source asset exchange, inside the receiver's route deadline.
const ASSET_DEADLINE: Duration = Duration::from_secs(32);

pub(crate) enum PeerFileAsset {
    Ready {
        bytes: axum::body::Bytes,
        mime: &'static str,
    },
    /// The Source answered 202: the overlay generation is still being made.
    Preparing,
}

/// The closed representation per asset kind: exact media type, B's byte cap
/// and a leading-bytes check the Source body must pass.
struct Representation {
    mime: &'static str,
    limit: usize,
    leading: fn(&[u8]) -> bool,
}
fn representation(kind: Kind) -> Option<Representation> {
    let (mime, limit, leading): (_, _, fn(&[u8]) -> bool) = match kind {
        Kind::Subtitle { .. } => ("text/vtt; charset=utf-8", MAX_VTT_BYTES, |bytes| {
            let text = bytes.strip_prefix("\u{feff}".as_bytes()).unwrap_or(bytes);
            text.starts_with(b"WEBVTT") && std::str::from_utf8(bytes).is_ok()
        }),
        Kind::SubtitleManifest { .. } => ("application/json", MAX_MANIFEST_BYTES, |bytes| {
            bytes.first() == Some(&b'{')
        }),
        Kind::SubtitleObject { .. } => ("image/png", MAX_IMAGE_BYTES, |bytes| {
            bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        }),
        Kind::ChapterThumbnail { .. } => ("image/jpeg", MAX_IMAGE_BYTES, |bytes| {
            bytes.starts_with(b"\xff\xd8\xff")
        }),
        Kind::Decision | Kind::Start | Kind::Direct | Kind::Progressive => return None,
    };
    Some(Representation {
        mime,
        limit,
        leading,
    })
}

impl PeerConnection {
    pub(crate) async fn file_asset(
        &mut self,
        credential: &Secret,
        target: &SourcePlaybackTarget,
        resource: &SharingFileResource,
    ) -> Result<PeerFileAsset, PeerError> {
        let Representation {
            mime,
            limit,
            leading,
        } = representation(resource.kind()).ok_or(PeerError::InvalidResponse)?;
        let reference = encode_reference(target).ok_or(PeerError::InvalidResponse)?;
        tokio::time::timeout(ASSET_DEADLINE, async {
            let mut authorization =
                HeaderValue::from_str(&format!("CinemaShare {}", credential.expose()))
                    .map_err(|_| PeerError::InvalidResponse)?;
            authorization.set_sensitive(true);
            let request = Request::builder()
                .method(Method::GET)
                .uri(format!(
                    "/sharing/v1/items/{}/files/{}/{}",
                    target.item_id.as_str(),
                    target.file_id.as_str(),
                    resource.as_str()
                ))
                .header(header::HOST, &self.host)
                .header(header::AUTHORIZATION, authorization)
                .header(header::ACCEPT_ENCODING, "identity")
                .header(REFERENCE_HEADER, reference)
                .body(Body::empty())
                .map_err(|_| PeerError::InvalidResponse)?;
            self.sender
                .ready()
                .await
                .map_err(|_| PeerError::Unavailable)?;
            let response = self
                .sender
                .send_request(request)
                .await
                .map_err(|_| PeerError::Unavailable)?;
            let status = response.status();
            if status != StatusCode::OK {
                // Bounded and discarded: a refusal's text is never relayed.
                axum::body::to_bytes(Body::new(response.into_body()), MANAGEMENT_RESPONSE_BYTES)
                    .await
                    .map_err(|_| PeerError::InvalidResponse)?;
                return match status {
                    StatusCode::ACCEPTED
                        if matches!(resource.kind(), Kind::SubtitleManifest { .. }) =>
                    {
                        Ok(PeerFileAsset::Preparing)
                    }
                    StatusCode::UNAUTHORIZED => Err(PeerError::Authentication),
                    _ => Err(PeerError::Rejected(status)),
                };
            }
            let headers = response.headers();
            let single = |name: header::HeaderName| {
                let mut values = headers.get_all(&name).iter();
                match (values.next(), values.next()) {
                    (Some(value), None) => value.to_str().map_err(|_| PeerError::InvalidResponse),
                    _ => Err(PeerError::InvalidResponse),
                }
            };
            let length_text = single(header::CONTENT_LENGTH)?;
            let length = length_text
                .parse::<usize>()
                .map_err(|_| PeerError::InvalidResponse)?;
            if single(header::CONTENT_TYPE)? != mime
                || length == 0
                || length > limit
                || length.to_string() != length_text
                || headers.contains_key(header::TRANSFER_ENCODING)
                || headers
                    .get_all(header::CONTENT_ENCODING)
                    .iter()
                    .any(|value| value != "identity")
            {
                return Err(PeerError::InvalidResponse);
            }
            let mut body = response.into_body();
            let mut bytes = Vec::with_capacity(length);
            while let Some(frame) = body.frame().await {
                let frame = frame.map_err(|_| PeerError::Unavailable)?;
                let Ok(data) = frame.into_data() else {
                    return Err(PeerError::InvalidResponse);
                };
                if data.len() > length - bytes.len() {
                    return Err(PeerError::InvalidResponse);
                }
                bytes.extend_from_slice(&data);
            }
            if bytes.len() != length || !leading(&bytes) {
                return Err(PeerError::InvalidResponse);
            }
            Ok(PeerFileAsset::Ready {
                bytes: axum::body::Bytes::from(bytes),
                mime,
            })
        })
        .await
        .map_err(|_| PeerError::Unavailable)?
    }
}

#[cfg(test)]
impl PeerConnection {
    /// A pinned-connection stand-in over an in-memory stream, for receiver
    /// tests that exercise the real Source router without a network.
    pub(crate) async fn over_test_stream<
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    >(
        stream: S,
    ) -> Result<Self, PeerError> {
        Self::from_stream(stream, "source.fixture.ts.net:32443".into()).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Bytes, http::Response};
    use http_body_util::{BodyExt, Full, StreamBody};
    use std::sync::{Arc, Mutex};

    type Seen = Arc<Mutex<Vec<(String, Option<String>, bool, Option<String>)>>>;

    /// One HTTP/1 exchange answered by `reply`, recording what B sent.
    async fn exchange(
        reply: impl Fn() -> Response<http_body_util::combinators::BoxBody<Bytes, std::convert::Infallible>>
            + Send
            + Sync
            + 'static,
        resource: &str,
    ) -> (Result<PeerFileAsset, PeerError>, Seen) {
        let target = target();
        let seen: Seen = Arc::default();
        let log = seen.clone();
        let reply = Arc::new(reply);
        let (client, server) = tokio::io::duplex(64 * 1024);
        let server = tokio::spawn(async move {
            let service =
                hyper::service::service_fn(move |request: Request<hyper::body::Incoming>| {
                    let log = log.clone();
                    let reply = reply.clone();
                    async move {
                        let text = |name: &str| {
                            request
                                .headers()
                                .get(name)
                                .and_then(|value| value.to_str().ok())
                                .map(str::to_owned)
                        };
                        log.lock().expect("log").push((
                            request.uri().path().to_owned(),
                            text(REFERENCE_HEADER),
                            request.headers().contains_key(header::AUTHORIZATION),
                            text("accept-encoding"),
                        ));
                        Ok::<_, std::convert::Infallible>(reply())
                    }
                });
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(server), service)
                .await;
        });
        let mut peer = PeerConnection::over_test_stream(client)
            .await
            .expect("fixture connection");
        let credential = plurx_core::sharing::new_secret().expect("credential");
        let resource = SharingFileResource::parse(resource).expect("resource");
        let result = peer.file_asset(&credential, &target, &resource).await;
        drop(peer);
        server.abort();
        let _ = server.await;
        (result, seen)
    }
    fn target() -> SourcePlaybackTarget {
        SourcePlaybackTarget {
            server_id: Uuid::new_v4(),
            catalogue_epoch: Uuid::new_v4(),
            library_id: SourceId::parse("9007199254740993").expect("library"),
            item_id: SourceId::parse("9223372036854775807").expect("item"),
            file_id: SourceId::parse("0").expect("file"),
            revision: plurx_core::sharing_catalogue_details::FileRevision::parse(&"d".repeat(64))
                .expect("revision"),
        }
    }
    fn full(
        status: StatusCode,
        headers: &[(&'static str, &str)],
        body: Vec<u8>,
    ) -> Response<http_body_util::combinators::BoxBody<Bytes, std::convert::Infallible>> {
        let mut response = Response::builder().status(status);
        for (name, value) in headers {
            response = response.header(*name, *value);
        }
        response
            .body(Full::new(Bytes::from(body)).boxed())
            .expect("fixture response")
    }

    #[tokio::test]
    async fn sharing_receiver_asset_bounded_body_and_mime() {
        let vtt = b"WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nx\n".to_vec();
        let jpeg = b"\xff\xd8\xff\xe0thumb".to_vec();
        let png = b"\x89PNG\r\n\x1a\nobject".to_vec();
        let object = format!(
            "subs/2/overlay/{}/objects/{}.png",
            "c".repeat(64),
            "b".repeat(64)
        );
        type Case = (
            &'static str,
            String,
            Box<
                dyn Fn() -> Response<
                        http_body_util::combinators::BoxBody<Bytes, std::convert::Infallible>,
                    > + Send
                    + Sync,
            >,
            Option<&'static str>,
        );
        let ok_vtt = vtt.clone();
        let cases: Vec<Case> = vec![
            (
                "valid vtt",
                "subs/3.vtt".into(),
                Box::new(move || {
                    full(
                        StatusCode::OK,
                        &[("content-type", "text/vtt; charset=utf-8")],
                        ok_vtt.clone(),
                    )
                }),
                Some("text/vtt; charset=utf-8"),
            ),
            (
                "wrong type",
                "subs/3.vtt".into(),
                {
                    let v = vtt.clone();
                    Box::new(move || {
                        full(StatusCode::OK, &[("content-type", "text/plain")], v.clone())
                    })
                },
                None,
            ),
            (
                "duplicate type",
                "subs/3.vtt".into(),
                {
                    let v = vtt.clone();
                    Box::new(move || {
                        full(
                            StatusCode::OK,
                            &[
                                ("content-type", "text/vtt; charset=utf-8"),
                                ("content-type", "text/vtt; charset=utf-8"),
                            ],
                            v.clone(),
                        )
                    })
                },
                None,
            ),
            (
                "over cap",
                "subs/3.vtt".into(),
                Box::new(|| {
                    let mut big = b"WEBVTT\n\n".to_vec();
                    big.resize(crate::http::shared_receiver_assets::MAX_VTT_BYTES + 1, b'x');
                    full(
                        StatusCode::OK,
                        &[("content-type", "text/vtt; charset=utf-8")],
                        big,
                    )
                }),
                None,
            ),
            (
                "encoded",
                "subs/3.vtt".into(),
                {
                    let v = vtt.clone();
                    Box::new(move || {
                        full(
                            StatusCode::OK,
                            &[
                                ("content-type", "text/vtt; charset=utf-8"),
                                ("content-encoding", "gzip"),
                            ],
                            v.clone(),
                        )
                    })
                },
                None,
            ),
            (
                "not webvtt",
                "subs/3.vtt".into(),
                Box::new(|| {
                    full(
                        StatusCode::OK,
                        &[("content-type", "text/vtt; charset=utf-8")],
                        b"<html>".to_vec(),
                    )
                }),
                None,
            ),
            (
                "unframed",
                "subs/3.vtt".into(),
                {
                    let v = vtt.clone();
                    Box::new(move || {
                        let frames =
                            futures_util::stream::iter([Ok::<_, std::convert::Infallible>(
                                hyper::body::Frame::data(Bytes::from(v.clone())),
                            )]);
                        Response::builder()
                            .status(StatusCode::OK)
                            .header("content-type", "text/vtt; charset=utf-8")
                            .body(StreamBody::new(frames).boxed())
                            .expect("chunked")
                    })
                },
                None,
            ),
            (
                "redirect",
                "subs/3.vtt".into(),
                Box::new(|| {
                    full(
                        StatusCode::TEMPORARY_REDIRECT,
                        &[("location", "https://outside.example.invalid/")],
                        Vec::new(),
                    )
                }),
                None,
            ),
            (
                "accepted vtt",
                "subs/3.vtt".into(),
                Box::new(|| full(StatusCode::ACCEPTED, &[], b"{}".to_vec())),
                None,
            ),
            (
                "png as jpeg",
                "chapters/1/thumb".into(),
                {
                    let p = png.clone();
                    Box::new(move || {
                        full(StatusCode::OK, &[("content-type", "image/jpeg")], p.clone())
                    })
                },
                None,
            ),
            (
                "valid jpeg",
                "chapters/1/thumb".into(),
                {
                    let j = jpeg.clone();
                    Box::new(move || {
                        full(StatusCode::OK, &[("content-type", "image/jpeg")], j.clone())
                    })
                },
                Some("image/jpeg"),
            ),
            (
                "oversized image",
                object.clone(),
                Box::new(|| {
                    let mut big = b"\x89PNG\r\n\x1a\n".to_vec();
                    big.resize(crate::http::shared_receiver_assets::MAX_IMAGE_BYTES + 1, 0);
                    full(StatusCode::OK, &[("content-type", "image/png")], big)
                }),
                None,
            ),
            (
                "valid png",
                object.clone(),
                {
                    let p = png.clone();
                    Box::new(move || {
                        full(StatusCode::OK, &[("content-type", "image/png")], p.clone())
                    })
                },
                Some("image/png"),
            ),
        ];
        for (name, resource, reply, expected) in cases {
            let (result, seen) = exchange(reply, &resource).await;
            match (result, expected) {
                (Ok(PeerFileAsset::Ready { mime, bytes }), Some(expected)) => {
                    assert_eq!(mime, expected, "{name}");
                    assert!(!bytes.is_empty(), "{name}");
                }
                (Err(_), None) => {}
                (Ok(_), _) | (Err(_), Some(_)) => panic!("unexpected outcome: {name}"),
            }
            let seen = seen.lock().expect("seen");
            assert_eq!(seen.len(), 1, "{name}: one request, no redirect follow");
            assert_eq!(
                seen[0].0,
                format!("/sharing/v1/items/9223372036854775807/files/0/{resource}"),
                "{name}"
            );
            let reference: SourcePlaybackTarget =
                serde_json::from_str(seen[0].1.as_deref().expect("reference header"))
                    .expect("closed reference");
            assert_eq!(reference.file_id.as_str(), "0", "{name}");
            assert!(seen[0].2, "{name}: authenticated");
            assert_eq!(seen[0].3.as_deref(), Some("identity"), "{name}");
        }
        let (preparing, _) = exchange(
            || {
                full(
                    StatusCode::ACCEPTED,
                    &[("content-type", "application/json")],
                    br#"{"state":"preparing","retry_after_ms":1000}"#.to_vec(),
                )
            },
            "subs/2/overlay.json",
        )
        .await;
        assert!(matches!(preparing, Ok(PeerFileAsset::Preparing)));
        let (refused, _) = exchange(
            || {
                full(
                    StatusCode::NOT_FOUND,
                    &[("content-type", "application/json")],
                    br#"{"error":"x"}"#.to_vec(),
                )
            },
            "subs/2/overlay.json",
        )
        .await;
        assert!(matches!(
            refused,
            Err(PeerError::Rejected(StatusCode::NOT_FOUND))
        ));
        // Only the four asset kinds have a representation.
        for suffix in ["direct", "stream.mp4", "decision", "hls/sessions"] {
            let (result, seen) = exchange(|| full(StatusCode::OK, &[], Vec::new()), suffix).await;
            assert!(result.is_err(), "{suffix}");
            assert!(
                seen.lock().expect("seen").is_empty(),
                "{suffix}: nothing sent"
            );
        }
    }
}
