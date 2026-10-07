//! Closed Shared direct-play wire: the Source Start reply, the B-to-Source
//! byte request, the Source byte response head and B's public Start reply.
//! No value here carries admission, producer or delivery authority; each
//! side re-derives the byte plan from its own inputs and they must agree.
use super::{
    hls::SourcePlaybackTarget,
    stream::{file_range_head, plan_file_range, FileRangePlan},
    DecodedSourceHlsStart,
};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use plurx_core::sharing_resources::SharingResourceUnsupported;
use serde::{Deserialize, Serialize};
use uuid::{Uuid, Variant};

type Result<T> = std::result::Result<T, SharingResourceUnsupported>;

/// The closed `presentation` value a Shared start uses for direct play. It
/// travels inside the ordinary `CreateSession`, so it is part of the B recipe
/// fingerprint and of the Source canonical request identity by construction.
pub(crate) const DIRECT_PRESENTATION: &str = "direct";
const MAX_SAFE: i64 = 9_007_199_254_740_991;
const MAX_START_BYTES: usize = 16 * 1024;
/// Longest single Range value relayed. Local direct play parses any length;
/// a longer value is refused by B before Source IO (431), never truncated.
pub(crate) const MAX_RANGE_VALUE_BYTES: usize = 16 * 1024;
/// The planned Content-Length of the represented answer. The Source byte
/// exchange is a POST, so a HEAD's planned length cannot ride in the real
/// Content-Length header without breaking the transport framing.
pub(crate) const PLANNED_LENGTH_HEADER: &str = "cinemashare-content-length";

/// Exactly the audio/video media types Local direct play's content type
/// table can name, plus its unknown-extension fallback.
pub(crate) const DIRECT_MIMES: &[&str] = &[
    "video/mp4",
    "video/webm",
    "video/x-matroska",
    "video/mp2t",
    "video/x-msvideo",
    "audio/mp4",
    "audio/aac",
    "audio/mpeg",
    "audio/flac",
    "audio/ogg",
    "audio/wav",
    "audio/x-ms-wma",
    "application/octet-stream",
];

fn v4(id: Uuid) -> bool {
    !id.is_nil() && id.get_version_num() == 4 && id.get_variant() == Variant::RFC4122
}

/// Whether a complete client `CreateSession` JSON asks for direct play.
pub(crate) fn session_presentation_is_direct(session: &serde_json::Value) -> bool {
    session
        .get("presentation")
        .and_then(serde_json::Value::as_str)
        == Some(DIRECT_PRESENTATION)
}

/// The Source's published direct session: its lineage and the represented
/// file's length and media type. No control URL is advertised because no
/// control exchange exists for direct play, Local or shared.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceDirectStart {
    pub session_id: String,
    pub control_epoch: i64,
    pub length: u64,
    pub mime: String,
}
impl SourceDirectStart {
    pub(crate) fn validate(&self) -> Result<()> {
        let session = Uuid::parse_str(&self.session_id).map_err(|_| SharingResourceUnsupported)?;
        if !v4(session)
            || session.to_string() != self.session_id
            || !(1..=MAX_SAFE).contains(&self.control_epoch)
            || self.length > MAX_SAFE as u64
            || !DIRECT_MIMES.contains(&self.mime.as_str())
        {
            return Err(SharingResourceUnsupported);
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DirectEnvelope {
    reference: SourcePlaybackTarget,
    incarnation_id: Uuid,
    direct: SourceDirectStart,
}

/// The complete Source direct Start/status envelope.
pub(crate) fn source_direct_envelope(
    reference: &SourcePlaybackTarget,
    incarnation_id: Uuid,
    direct: &SourceDirectStart,
) -> serde_json::Value {
    serde_json::json!({"reference":reference,"incarnation_id":incarnation_id,"direct":direct})
}

/// Strictly decoded facts of a Source direct reply; never producer proof.
pub(crate) struct DecodedSourceDirectStart(DirectEnvelope);
impl DecodedSourceDirectStart {
    pub(crate) fn parse(bytes: &[u8], expected: &SourcePlaybackTarget) -> Result<Self> {
        if bytes.is_empty() || bytes.len() > MAX_START_BYTES {
            return Err(SharingResourceUnsupported);
        }
        let mut json = serde_json::Deserializer::from_slice(bytes);
        let value = super::sharing_decision_decode::bounded_decision_value(&mut json)
            .map_err(|_| SharingResourceUnsupported)?;
        json.end().map_err(|_| SharingResourceUnsupported)?;
        let envelope: DirectEnvelope =
            serde_json::from_value(value.clone()).map_err(|_| SharingResourceUnsupported)?;
        if &envelope.reference != expected
            || expected.server_id.is_nil()
            || expected.catalogue_epoch.is_nil()
            || !v4(envelope.incarnation_id)
            || serde_json::to_value(&envelope).map_err(|_| SharingResourceUnsupported)? != value
        {
            return Err(SharingResourceUnsupported);
        }
        envelope.direct.validate()?;
        Ok(Self(envelope))
    }
    #[cfg(test)]
    pub(crate) fn incarnation_id(&self) -> Uuid {
        self.0.incarnation_id
    }
    #[cfg(test)]
    pub(crate) fn direct(&self) -> &SourceDirectStart {
        &self.0.direct
    }
    pub(crate) fn into_parts(self) -> (SourcePlaybackTarget, Uuid, SourceDirectStart) {
        (self.0.reference, self.0.incarnation_id, self.0.direct)
    }
}

/// A decoded Source Start of the presentation B asked for. B decodes only
/// the presentation its own retained recipe names; the other is refused.
pub(crate) enum DecodedSourceStart {
    Hls(Box<DecodedSourceHlsStart>),
    Direct(DecodedSourceDirectStart),
}
pub(crate) fn decode_source_start(
    bytes: &[u8],
    expected: &SourcePlaybackTarget,
    direct: bool,
) -> Result<DecodedSourceStart> {
    if direct {
        DecodedSourceDirectStart::parse(bytes, expected).map(DecodedSourceStart::Direct)
    } else {
        super::decode_source_start_response(bytes, expected)
            .map(|decoded| DecodedSourceStart::Hls(Box::new(decoded)))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub(crate) enum DirectMethod {
    Get,
    Head,
}

/// The viewer's byte demand as B relays it: the method, at most the first two
/// Range values (any second value already makes a bytes Range invalid) and
/// whether If-Range was present (its value is never consulted, Local or
/// shared). A Range value that is not visible ASCII is relayed as the empty
/// value, which Local's parser refuses exactly as it refuses the original.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DirectByteRequest {
    pub method: DirectMethod,
    pub range: Vec<String>,
    pub if_range: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DirectRequestRefusal {
    Method,
    RangeTooLarge,
}
impl DirectByteRequest {
    pub(crate) fn from_request(
        method: &Method,
        headers: &HeaderMap,
    ) -> std::result::Result<Self, DirectRequestRefusal> {
        let method = match *method {
            Method::GET => DirectMethod::Get,
            Method::HEAD => DirectMethod::Head,
            _ => return Err(DirectRequestRefusal::Method),
        };
        let mut range = Vec::new();
        for value in headers.get_all(header::RANGE).iter().take(2) {
            if value.len() > MAX_RANGE_VALUE_BYTES {
                return Err(DirectRequestRefusal::RangeTooLarge);
            }
            range.push(value.to_str().map(str::to_owned).unwrap_or_default());
        }
        Ok(Self {
            method,
            range,
            if_range: headers.contains_key(header::IF_RANGE),
        })
    }
    pub(crate) fn method(&self) -> Method {
        match self.method {
            DirectMethod::Get => Method::GET,
            DirectMethod::Head => Method::HEAD,
        }
    }
    /// The request headers Local's planner reads, rebuilt from the relayed
    /// demand. Refuses anything B could not have produced.
    pub(crate) fn headers(&self) -> std::result::Result<HeaderMap, SharingResourceUnsupported> {
        if self.range.len() > 2 {
            return Err(SharingResourceUnsupported);
        }
        let mut headers = HeaderMap::new();
        for value in &self.range {
            if value.len() > MAX_RANGE_VALUE_BYTES {
                return Err(SharingResourceUnsupported);
            }
            let value = HeaderValue::from_str(value).map_err(|_| SharingResourceUnsupported)?;
            if !value.is_empty() && value.to_str().is_err() {
                return Err(SharingResourceUnsupported);
            }
            headers.append(header::RANGE, value);
        }
        if self.if_range {
            headers.insert(header::IF_RANGE, HeaderValue::from_static("*"));
        }
        Ok(headers)
    }
    /// The same plan Local direct play computes for the original request.
    pub(crate) fn plan(
        &self,
        length: u64,
    ) -> std::result::Result<FileRangePlan, SharingResourceUnsupported> {
        Ok(plan_file_range(&self.headers()?, &self.method(), length))
    }
}

/// Bytes the represented answer carries on the wire for this plan.
pub(crate) fn planned_body_length(plan: FileRangePlan, method: &Method, length: u64) -> u64 {
    match plan {
        FileRangePlan::Unsatisfiable => 0,
        FileRangePlan::Full if method == Method::HEAD => 0,
        FileRangePlan::Full => length,
        FileRangePlan::Partial { start, end } => end - start + 1,
    }
}

fn single<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let mut values = headers.get_all(name).iter();
    let value = values.next()?;
    if values.next().is_some() {
        return None;
    }
    value.to_str().ok()
}

/// A Source direct response head that B has verified against its own plan:
/// the exact Local status and header set for this request and this file.
#[derive(Debug)]
pub(crate) struct SourceDirectHead {
    pub(crate) status: StatusCode,
    pub(crate) headers: Vec<(HeaderName, String)>,
    pub(crate) body_length: u64,
}
impl SourceDirectHead {
    pub(crate) fn parse(
        status: StatusCode,
        headers: &HeaderMap,
        echo: &[(&'static str, String)],
        request: &DirectByteRequest,
        length: u64,
        mime: &str,
    ) -> std::result::Result<Self, SharingResourceUnsupported> {
        for (name, value) in echo {
            if single(headers, name) != Some(value.as_str()) {
                return Err(SharingResourceUnsupported);
            }
        }
        if headers.contains_key(header::TRANSFER_ENCODING)
            || headers.contains_key(header::CONTENT_ENCODING)
            || headers.contains_key(header::TRAILER)
            || single(headers, "cache-control") != Some("no-store")
        {
            return Err(SharingResourceUnsupported);
        }
        let method = request.method();
        let plan = request.plan(length)?;
        let (expected_status, expected) = file_range_head(plan, length, mime);
        if status != expected_status {
            return Err(SharingResourceUnsupported);
        }
        for (name, value) in &expected {
            let actual = if name == header::CONTENT_LENGTH {
                single(headers, PLANNED_LENGTH_HEADER)
            } else {
                single(headers, name.as_str())
            };
            if actual != Some(value.as_str()) {
                return Err(SharingResourceUnsupported);
            }
        }
        for name in [header::CONTENT_TYPE, header::CONTENT_RANGE] {
            if headers.contains_key(&name) && !expected.iter().any(|(n, _)| *n == name) {
                return Err(SharingResourceUnsupported);
            }
        }
        let body_length = planned_body_length(plan, &method, length);
        if single(headers, "content-length") != Some(body_length.to_string().as_str()) {
            return Err(SharingResourceUnsupported);
        }
        Ok(Self {
            status,
            headers: expected,
            body_length,
        })
    }
}

/// B's public reply to an authenticated direct Shared start. The URL is the
/// B file alias with the bounded B session binding; nothing names the Source.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SharedDirectStart {
    pub presentation: String,
    pub session_id: String,
    pub url: String,
    pub length: u64,
    pub mime: String,
}
impl SharedDirectStart {
    pub(crate) fn new(
        file_base: &str,
        session_id: Uuid,
        direct: &SourceDirectStart,
    ) -> Result<Self> {
        direct.validate()?;
        if !v4(session_id)
            || !file_base.starts_with("/api/v1/shared/imports/")
            || file_base.contains(['?', '#'])
        {
            return Err(SharingResourceUnsupported);
        }
        Ok(Self {
            presentation: DIRECT_PRESENTATION.to_owned(),
            session_id: session_id.to_string(),
            url: format!("{file_base}/direct?session={session_id}"),
            length: direct.length,
            mime: direct.mime.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local_headers(method: &Method, values: &[&[u8]], if_range: bool) -> HeaderMap {
        let _ = method;
        let mut headers = HeaderMap::new();
        for value in values {
            headers.append(
                header::RANGE,
                HeaderValue::from_bytes(value).expect("fixture header"),
            );
        }
        if if_range {
            headers.insert(header::IF_RANGE, HeaderValue::from_static("\"tag\""));
        }
        headers
    }

    #[test]
    fn sharing_direct_wire_range_relay_preserves_the_local_plan() {
        let cases: Vec<Vec<&[u8]>> = vec![
            vec![],
            vec![b"bytes=0-0"],
            vec![b"bytes=2-4"],
            vec![b"bytes=-3"],
            vec![b"bytes=7-"],
            vec![b"bytes=99-"],
            vec![b"bytes=99-,2-3"],
            vec![b"bytes=0-1,bad"],
            vec![b"items=0-1"],
            vec![b"items=0-1", b"bytes=0-1"],
            vec![b"bytes=0-1", b"bytes=2-3"],
            vec![b"bytes=0-1", b"bytes=2-3", b"bytes=4-5"],
            vec![b"bytes=0-\x801"],
            vec![b"bytes=\t0-1"],
            vec![b""],
            vec![b"bytes=0-99999999999999999999999999999"],
            vec![b"Bytes=00001-00002"],
        ];
        for length in [0_u64, 1, 10, 4096] {
            for values in &cases {
                for method in [Method::GET, Method::HEAD] {
                    for if_range in [false, true] {
                        let original = local_headers(&method, values, if_range);
                        let local = plan_file_range(&original, &method, length);
                        let relayed =
                            DirectByteRequest::from_request(&method, &original).expect("relayable");
                        let wire: DirectByteRequest =
                            serde_json::from_slice(&serde_json::to_vec(&relayed).expect("wire"))
                                .expect("decoded");
                        assert_eq!(
                            wire.plan(length).expect("rebuilt"),
                            local,
                            "{values:?} {method} if_range={if_range} len={length}"
                        );
                    }
                }
            }
        }
        assert_eq!(
            DirectByteRequest::from_request(&Method::POST, &HeaderMap::new()),
            Err(DirectRequestRefusal::Method)
        );
        let long = format!("bytes=0-{}", "0".repeat(MAX_RANGE_VALUE_BYTES));
        assert_eq!(
            DirectByteRequest::from_request(
                &Method::GET,
                &local_headers(&Method::GET, &[long.as_bytes()], false)
            ),
            Err(DirectRequestRefusal::RangeTooLarge)
        );
        let forged = DirectByteRequest {
            method: DirectMethod::Get,
            range: vec!["a".into(), "b".into(), "c".into()],
            if_range: false,
        };
        assert!(forged.headers().is_err());
        let control = DirectByteRequest {
            method: DirectMethod::Get,
            range: vec!["bytes=0-1\r\n".into()],
            if_range: false,
        };
        assert!(control.headers().is_err());
    }

    fn echo() -> Vec<(&'static str, String)> {
        vec![("cinemashare-session-id", "s".into())]
    }
    fn source_head(status: StatusCode, pairs: &[(&str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("cinemashare-session-id", HeaderValue::from_static("s"));
        headers.insert("cache-control", HeaderValue::from_static("no-store"));
        for (name, value) in pairs {
            headers.append(
                HeaderName::from_bytes(name.as_bytes()).expect("name"),
                HeaderValue::from_str(value).expect("value"),
            );
        }
        let _ = status;
        headers
    }

    #[test]
    fn sharing_receiver_direct_relays_206_416_headers_exactly() {
        let get = |range: &[&str], if_range| DirectByteRequest {
            method: DirectMethod::Get,
            range: range.iter().map(|r| (*r).to_owned()).collect(),
            if_range,
        };
        let partial = source_head(
            StatusCode::PARTIAL_CONTENT,
            &[
                ("content-type", "video/mp4"),
                ("accept-ranges", "bytes"),
                ("cinemashare-content-length", "3"),
                ("content-length", "3"),
                ("content-range", "bytes 2-4/10"),
            ],
        );
        let head = SourceDirectHead::parse(
            StatusCode::PARTIAL_CONTENT,
            &partial,
            &echo(),
            &get(&["bytes=2-4"], false),
            10,
            "video/mp4",
        )
        .expect("exact Local 206");
        assert_eq!(head.body_length, 3);
        assert_eq!(
            head.headers,
            file_range_head(FileRangePlan::Partial { start: 2, end: 4 }, 10, "video/mp4").1
        );
        // The same 206 is refused when B's own plan disagrees with it.
        for (request, length, mime) in [
            (get(&["bytes=2-5"], false), 10, "video/mp4"),
            (get(&["bytes=2-4"], true), 10, "video/mp4"),
            (get(&["bytes=2-4"], false), 11, "video/mp4"),
            (get(&["bytes=2-4"], false), 10, "video/webm"),
        ] {
            assert!(SourceDirectHead::parse(
                StatusCode::PARTIAL_CONTENT,
                &partial,
                &echo(),
                &request,
                length,
                mime
            )
            .is_err());
        }
        let unsatisfiable = source_head(
            StatusCode::RANGE_NOT_SATISFIABLE,
            &[
                ("content-range", "bytes */10"),
                ("accept-ranges", "bytes"),
                ("cinemashare-content-length", "0"),
                ("content-length", "0"),
            ],
        );
        let head = SourceDirectHead::parse(
            StatusCode::RANGE_NOT_SATISFIABLE,
            &unsatisfiable,
            &echo(),
            &get(&["bytes=99-"], false),
            10,
            "video/mp4",
        )
        .expect("exact Local 416");
        assert_eq!(head.body_length, 0);
        let mut typed = unsatisfiable.clone();
        typed.insert("content-type", HeaderValue::from_static("video/mp4"));
        assert!(SourceDirectHead::parse(
            StatusCode::RANGE_NOT_SATISFIABLE,
            &typed,
            &echo(),
            &get(&["bytes=99-"], false),
            10,
            "video/mp4",
        )
        .is_err());
        // HEAD: the represented length rides in the planned header only.
        let head_reply = source_head(
            StatusCode::OK,
            &[
                ("content-type", "video/mp4"),
                ("accept-ranges", "bytes"),
                ("cinemashare-content-length", "10"),
                ("content-length", "0"),
            ],
        );
        let head = SourceDirectHead::parse(
            StatusCode::OK,
            &head_reply,
            &echo(),
            &DirectByteRequest {
                method: DirectMethod::Head,
                range: vec!["bytes=2-4".into()],
                if_range: false,
            },
            10,
            "video/mp4",
        )
        .expect("HEAD never ranges");
        assert_eq!(head.body_length, 0);
        assert_eq!(head.headers[2], (header::CONTENT_LENGTH, "10".to_owned()));
        for (name, value) in [
            ("transfer-encoding", "chunked"),
            ("content-encoding", "gzip"),
            ("cinemashare-session-id", "other"),
        ] {
            let mut drift = partial.clone();
            drift.insert(
                HeaderName::from_static(name),
                HeaderValue::from_static(value),
            );
            assert!(SourceDirectHead::parse(
                StatusCode::PARTIAL_CONTENT,
                &drift,
                &echo(),
                &get(&["bytes=2-4"], false),
                10,
                "video/mp4",
            )
            .is_err());
        }
    }

    #[test]
    fn sharing_direct_start_wire_is_closed_and_exact() {
        let reference = SourcePlaybackTarget {
            server_id: Uuid::new_v4(),
            catalogue_epoch: Uuid::new_v4(),
            library_id: plurx_core::sharing::SourceId::parse("0").expect("library"),
            item_id: plurx_core::sharing::SourceId::parse("9007199254740993").expect("item"),
            file_id: plurx_core::sharing::SourceId::parse("1").expect("file"),
            revision: plurx_core::sharing_catalogue_details::FileRevision::parse(&"a".repeat(64))
                .expect("revision"),
        };
        let direct = SourceDirectStart {
            session_id: Uuid::new_v4().to_string(),
            control_epoch: 1,
            length: 10,
            mime: "video/mp4".into(),
        };
        let incarnation = Uuid::new_v4();
        let bytes = serde_json::to_vec(&source_direct_envelope(&reference, incarnation, &direct))
            .expect("envelope");
        let decoded = DecodedSourceDirectStart::parse(&bytes, &reference).expect("exact");
        assert_eq!(decoded.direct(), &direct);
        assert_eq!(decoded.incarnation_id(), incarnation);
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        for (path, drift) in [
            ("mime", serde_json::json!("text/html")),
            ("control_epoch", serde_json::json!(0)),
            ("session_id", serde_json::json!("not-a-uuid")),
            ("extra", serde_json::json!(1)),
        ] {
            let mut bad = value.clone();
            bad["direct"][path] = drift;
            assert!(DecodedSourceDirectStart::parse(
                &serde_json::to_vec(&bad).expect("bad"),
                &reference
            )
            .is_err());
        }
        value["response"] = serde_json::json!({});
        assert!(DecodedSourceDirectStart::parse(
            &serde_json::to_vec(&value).expect("hls shape"),
            &reference
        )
        .is_err());
        assert!(decode_source_start(&bytes, &reference, false).is_err());
        let b = Uuid::new_v4();
        let public =
            SharedDirectStart::new("/api/v1/shared/imports/i/files/l", b, &direct).expect("B");
        assert_eq!(
            public.url,
            format!("/api/v1/shared/imports/i/files/l/direct?session={b}")
        );
        assert!(SharedDirectStart::new("/api/v1/files/1", b, &direct).is_err());
    }

    #[test]
    fn sharing_protocol_fixture_direct_start() {
        use crate::sharing_protocol_fixture::{accepted, b_layer, fixture, mutated, rows};
        let fixture = fixture();
        let direct = &fixture["direct"];
        let mimes = rows(&direct["mimes"], "direct MIME")
            .iter()
            .map(|mime| mime.as_str().expect("MIME"))
            .collect::<Vec<_>>();
        assert_eq!(mimes, DIRECT_MIMES);
        let file_base = fixture["context"]["file_base"].as_str().expect("file base");
        let b = direct["b_session"].as_str().expect("B session");
        let session = Uuid::parse_str(b).expect("B session");
        let public = |source: &serde_json::Value| {
            serde_json::from_value::<SourceDirectStart>(source.clone())
                .ok()
                .and_then(|source| SharedDirectStart::new(file_base, session, &source).ok())
                .map(|public| serde_json::to_value(public).expect("public wire"))
        };
        assert_eq!(public(&direct["source"]), Some(direct["public"].clone()));
        let source_session = direct["source"]["session_id"].as_str().expect("session");
        let mut checked = 0;
        for row in rows(&direct["mutations"], "direct mutation") {
            if !b_layer(row) {
                continue;
            }
            let projected = public(&mutated(&direct["source"], row, source_session));
            assert_eq!(projected.is_some(), accepted(row), "{}", row["id"]);
            if row["layer"] == "both" && accepted(row) {
                assert_eq!(
                    projected,
                    Some(mutated(&direct["public"], row, b)),
                    "{}",
                    row["id"]
                );
            }
            checked += 1;
        }
        assert!(checked >= 9, "direct rows the receiver validates");
    }
}
