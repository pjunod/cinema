//! Closed Source session RPC data. Constructors convey no producer authority.
use super::*;
use crate::http::hls::{SourcePlaybackTarget, StartResponse};
use serde_json::{json, Value};

const MAX_REPLY: usize = 16 * 1024;
const MAX_EPOCH: i64 = 9_007_199_254_740_991;
/// The Source answers End within its own budget: its End owner waits up to
/// 305 s and its peer router bounds the route at 310 s, after which a
/// repeated End reattaches the same owner. B outwaits that whole budget plus
/// transit, so a slow Source settlement is answered once rather than timed
/// out into another dial.
const SOURCE_END_DEADLINE: Duration = Duration::from_secs(315);

/// No Deref or access to catalogue, Start or resource methods. The retained
/// approved SPKI authenticates TLS; the exact authenticated End echo binds
/// the original Source identity even while its identity route is disabled.
pub(crate) struct CleanupPeerConnection(PeerConnection);
impl CleanupPeerConnection {
    pub(crate) async fn connect(
        manager: &SharingManager,
        endpoint: &Endpoint,
    ) -> Result<Self, PeerError> {
        PeerConnection::connect_pinned(manager, std::slice::from_ref(endpoint))
            .await
            .map(Self)
    }
    pub(crate) async fn end(
        &mut self,
        credential: &Secret,
        viewer_hash: &str,
        session: &SourcePeerSession,
        known: Option<&SourcePeerLineage>,
    ) -> Result<SourceEndReceipt, PeerError> {
        self.0
            .file_end(credential, viewer_hash, session, known)
            .await
    }
}

pub(crate) struct SourcePeerSession {
    reference: SourcePlaybackTarget,
    request_id: Uuid,
    session: Value,
}
impl SourcePeerSession {
    pub(crate) fn new(
        reference: SourcePlaybackTarget,
        canonical_start: &[u8],
    ) -> Result<Self, PeerError> {
        crate::http::validate_source_start_request(canonical_start, &reference)
            .map_err(|_| PeerError::InvalidResponse)?;
        let value: Value =
            serde_json::from_slice(canonical_start).map_err(|_| PeerError::InvalidResponse)?;
        let session = value
            .get("session")
            .ok_or(PeerError::InvalidResponse)?
            .clone();
        let request_id = session
            .get("request_id")
            .and_then(Value::as_str)
            .and_then(|id| Uuid::parse_str(id).ok())
            .ok_or(PeerError::InvalidResponse)?;
        Ok(Self {
            reference,
            request_id,
            session,
        })
    }
    fn end_body(&self, lineage: Option<&SourcePeerLineage>) -> Result<Vec<u8>, PeerError> {
        let mut value = json!({"reference":self.reference,"session":self.session});
        if let Some(lineage) = lineage {
            value["incarnation_id"] = json!(lineage.incarnation_id);
            value["session_id"] = json!(lineage.session_id);
            value["control_epoch"] = json!(lineage.control_epoch);
        }
        let bytes = serde_json::to_vec(&value).map_err(|_| PeerError::InvalidResponse)?;
        if bytes.len() > 128 * 1024 {
            return Err(PeerError::InvalidResponse);
        }
        Ok(bytes)
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct SourcePeerLineage {
    incarnation_id: Uuid,
    session_id: Uuid,
    control_epoch: i64,
}
impl SourcePeerLineage {
    pub(crate) fn from_start(
        incarnation_id: Uuid,
        response: &StartResponse,
    ) -> Result<Self, PeerError> {
        let session_id =
            Uuid::parse_str(&response.session_id).map_err(|_| PeerError::InvalidResponse)?;
        let control_epoch = i64::try_from(
            response
                .control
                .as_ref()
                .ok_or(PeerError::InvalidResponse)?
                .control_epoch,
        )
        .map_err(|_| PeerError::InvalidResponse)?;
        if !v4(incarnation_id)
            || !v4(session_id)
            || session_id.to_string() != response.session_id
            || !(1..=MAX_EPOCH).contains(&control_epoch)
        {
            return Err(PeerError::InvalidResponse);
        }
        Ok(Self {
            incarnation_id,
            session_id,
            control_epoch,
        })
    }
}

impl SourcePeerLineage {
    /// The same lineage checks as [`Self::from_start`], applied to the facts
    /// a recovered owner opened from its sealed upstream capsule.
    pub(crate) fn from_capsule(
        incarnation_id: Uuid,
        session_id: Uuid,
        control_epoch: u64,
    ) -> Result<Self, PeerError> {
        let control_epoch = i64::try_from(control_epoch).map_err(|_| PeerError::InvalidResponse)?;
        if !v4(incarnation_id) || !v4(session_id) || !(1..=MAX_EPOCH).contains(&control_epoch) {
            return Err(PeerError::InvalidResponse);
        }
        Ok(Self {
            incarnation_id,
            session_id,
            control_epoch,
        })
    }
}

/// Verified bounded reply facts from an authenticated pinned Source exchange.
/// B must also join its own accepted bodies/tasks before minting retirement
/// evidence. JSON decoding alone cannot establish physical settlement.
pub(crate) struct SourceEndReceipt(EndReply);
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EndReply {
    reference: SourcePlaybackTarget,
    #[serde(deserialize_with = "canonical_uuid")]
    request_id: Uuid,
    #[serde(deserialize_with = "canonical_uuid")]
    incarnation_id: Uuid,
    session_id: Option<Uuid>,
    control_epoch: Option<i64>,
    state: String,
    #[serde(deserialize_with = "canonical_uuid")]
    confirmation_id: Uuid,
    settled: bool,
}
impl SourceEndReceipt {
    fn parse(
        bytes: &[u8],
        expected: &SourcePeerSession,
        known: Option<&SourcePeerLineage>,
    ) -> Result<Self, PeerError> {
        if bytes.is_empty() || bytes.len() > MAX_REPLY {
            return Err(PeerError::InvalidResponse);
        }
        let mut decoder = serde_json::Deserializer::from_slice(bytes);
        let value = crate::http::bounded_decision_value(&mut decoder)
            .map_err(|_| PeerError::InvalidResponse)?;
        decoder.end().map_err(|_| PeerError::InvalidResponse)?;
        let reply: EndReply =
            serde_json::from_value(value.clone()).map_err(|_| PeerError::InvalidResponse)?;
        if reply.reference != expected.reference
            || value.get("reference")
                != Some(
                    &serde_json::to_value(&expected.reference)
                        .map_err(|_| PeerError::InvalidResponse)?,
                )
            || reply.request_id != expected.request_id
            || !v4(reply.request_id)
            || !v4(reply.incarnation_id)
            || !v4(reply.confirmation_id)
            || !reply.settled
            || reply.state != "settled"
        {
            return Err(PeerError::InvalidResponse);
        }
        if value.get("session_id").is_some_and(Value::is_null)
            || value.get("control_epoch").is_some_and(Value::is_null)
        {
            return Err(PeerError::InvalidResponse);
        }
        let observed = match (reply.session_id, reply.control_epoch) {
            (None, None) => None,
            (Some(session_id), Some(control_epoch))
                if v4(session_id) && (1..=MAX_EPOCH).contains(&control_epoch) =>
            {
                if value.get("session_id").and_then(Value::as_str)
                    != Some(session_id.to_string().as_str())
                {
                    return Err(PeerError::InvalidResponse);
                }
                Some(SourcePeerLineage {
                    incarnation_id: reply.incarnation_id,
                    session_id,
                    control_epoch,
                })
            }
            _ => return Err(PeerError::InvalidResponse),
        };
        if known.is_some_and(|known| observed.as_ref() != Some(known)) {
            return Err(PeerError::InvalidResponse);
        }
        Ok(Self(reply))
    }
    pub(crate) fn confirmation_id(&self) -> Uuid {
        self.0.confirmation_id
    }
    pub(crate) fn incarnation_id(&self) -> Uuid {
        self.0.incarnation_id
    }
    pub(crate) fn retirement_confirmation(
        &self,
        session: &SourcePeerSession,
    ) -> Result<String, PeerError> {
        use sha2::{Digest, Sha256};
        if self.0.reference != session.reference || self.0.request_id != session.request_id {
            return Err(PeerError::InvalidResponse);
        }
        let identity = serde_json::to_vec(&json!({
            "reference":self.0.reference,"request":self.0.request_id,
            "incarnation":self.0.incarnation_id,"session":self.0.session_id,
            "epoch":self.0.control_epoch,"confirmation":self.0.confirmation_id,
        }))
        .map_err(|_| PeerError::InvalidResponse)?;
        let mut digest = Sha256::new();
        digest.update(b"plurx.receiver.source-end-confirmation.v1\0");
        digest.update(identity);
        Ok(format!("{:x}", digest.finalize()))
    }
}
fn v4(id: Uuid) -> bool {
    !id.is_nil() && id.get_version_num() == 4 && id.get_variant() == uuid::Variant::RFC4122
}

impl PeerConnection {
    /// One dedicated pinned cleanup exchange. Timeouts retain the caller's
    /// obligation; they never synthesize an End acknowledgement or retry.
    pub(crate) async fn file_end(
        &mut self,
        credential: &Secret,
        viewer_hash: &str,
        session: &SourcePeerSession,
        known: Option<&SourcePeerLineage>,
    ) -> Result<SourceEndReceipt, PeerError> {
        if self.verified_endpoint.is_none()
            || viewer_hash.len() != 64
            || !viewer_hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(PeerError::InvalidResponse);
        }
        let body = session.end_body(known)?;
        tokio::time::timeout(SOURCE_END_DEADLINE, async {
            let mut auth = HeaderValue::from_str(&format!("CinemaShare {}", credential.expose()))
                .map_err(|_| PeerError::InvalidResponse)?;
            auth.set_sensitive(true);
            let mut viewer =
                HeaderValue::from_str(viewer_hash).map_err(|_| PeerError::InvalidResponse)?;
            viewer.set_sensitive(true);
            let request = Request::builder()
                .method(Method::POST)
                .uri(format!(
                    "/sharing/v1/items/{}/files/{}/sessions/{}/end",
                    session.reference.item_id.as_str(),
                    session.reference.file_id.as_str(),
                    session.request_id
                ))
                .header(header::HOST, &self.host)
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::ACCEPT, "application/json")
                .header(header::AUTHORIZATION, auth)
                .header("cinemashare-viewer", viewer)
                .body(Body::from(body))
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
            let bytes = axum::body::to_bytes(
                Body::new(
                    response
                        .into_body()
                        .map_err(|_| std::io::Error::other("sharing peer cleanup body")),
                ),
                MAX_REPLY,
            )
            .await
            .map_err(|_| PeerError::InvalidResponse)?;
            if status == StatusCode::UNAUTHORIZED {
                return Err(PeerError::Authentication);
            }
            if status == StatusCode::UPGRADE_REQUIRED {
                return Err(PeerError::ProtocolUnsupported);
            }
            if status != StatusCode::OK {
                return Err(PeerError::Rejected(status));
            }
            SourceEndReceipt::parse(&bytes, session, known)
        })
        .await
        .map_err(|_| PeerError::Unavailable)?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plurx_core::sharing_catalogue_details::FileRevision;

    fn fixture() -> (SourcePeerSession, SourcePeerLineage, Value) {
        let reference = SourcePlaybackTarget {
            server_id: Uuid::new_v4(),
            catalogue_epoch: Uuid::new_v4(),
            library_id: SourceId::parse("0").expect("valid cleanup fixture"),
            item_id: SourceId::parse("9007199254740993").expect("valid cleanup fixture"),
            file_id: SourceId::parse("9223372036854775807").expect("valid cleanup fixture"),
            revision: FileRevision::parse(&"a".repeat(64)).expect("valid cleanup fixture"),
        };
        let request_id = Uuid::new_v4();
        let original = json!({"reference":reference,"session":{
            "request_id":request_id,"playback_id":"original-client",
            "quality":"1080p","start_seconds":0,"caps":{"hevc":false},
            "subtitle_stream":3
        }});
        let session = SourcePeerSession::new(
            reference.clone(),
            &serde_json::to_vec(&original).expect("valid cleanup fixture"),
        )
        .expect("valid cleanup fixture");
        let lineage = SourcePeerLineage {
            incarnation_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            control_epoch: 7,
        };
        let reply = json!({"reference":reference,"request_id":request_id,
            "incarnation_id":lineage.incarnation_id,"session_id":lineage.session_id,
            "control_epoch":lineage.control_epoch,"state":"settled",
            "confirmation_id":Uuid::new_v4(),"settled":true});
        (session, lineage, reply)
    }

    #[test]
    fn source_end_retains_complete_private_recipe_and_requires_exact_lineage() {
        let (session, lineage, reply) = fixture();
        let body: Value = serde_json::from_slice(
            &session
                .end_body(Some(&lineage))
                .expect("valid cleanup fixture"),
        )
        .expect("valid cleanup fixture");
        assert_eq!(body["session"], session.session);
        assert_eq!(body["session"]["subtitle_stream"], 3);
        assert_eq!(body["incarnation_id"], json!(lineage.incarnation_id));
        let bytes = serde_json::to_vec(&reply).expect("valid cleanup fixture");
        let receipt = SourceEndReceipt::parse(&bytes, &session, Some(&lineage))
            .expect("valid cleanup fixture");
        assert_eq!(receipt.incarnation_id(), lineage.incarnation_id);
        assert_eq!(
            receipt.confirmation_id().to_string(),
            reply["confirmation_id"]
                .as_str()
                .expect("valid cleanup fixture")
        );
        for field in [
            "request_id",
            "incarnation_id",
            "session_id",
            "confirmation_id",
        ] {
            let mut changed = reply.clone();
            changed[field] = json!(if field == "confirmation_id" {
                Uuid::nil()
            } else {
                Uuid::new_v4()
            });
            assert!(
                SourceEndReceipt::parse(
                    &serde_json::to_vec(&changed).expect("valid cleanup fixture"),
                    &session,
                    Some(&lineage)
                )
                .is_err(),
                "{field}"
            );
        }
        let mut changed = reply.clone();
        changed["reference"]["revision"] = json!("b".repeat(64));
        assert!(SourceEndReceipt::parse(
            &serde_json::to_vec(&changed).expect("valid cleanup fixture"),
            &session,
            Some(&lineage)
        )
        .is_err());
    }

    #[test]
    fn source_end_rejects_ambiguous_or_unsettled_replies_and_allows_lost_start() {
        let (session, lineage, reply) = fixture();
        for (key, value) in [
            ("settled", json!(false)),
            ("state", json!("retiring")),
            ("control_epoch", json!(0)),
            ("control_epoch", json!(MAX_EPOCH + 1)),
            (
                "session_id",
                json!(lineage.session_id.to_string().to_uppercase()),
            ),
            ("session_id", Value::Null),
            ("control_epoch", Value::Null),
            ("unexpected", json!(true)),
        ] {
            let mut changed = reply.clone();
            changed[key] = value;
            assert!(
                SourceEndReceipt::parse(
                    &serde_json::to_vec(&changed).expect("valid cleanup fixture"),
                    &session,
                    None
                )
                .is_err(),
                "{key}"
            );
        }
        let bytes = serde_json::to_string(&reply).expect("valid cleanup fixture");
        let mut changed_reference = reply.clone();
        changed_reference["reference"]["server_id"] =
            json!(session.reference.server_id.to_string().to_uppercase());
        assert!(SourceEndReceipt::parse(
            &serde_json::to_vec(&changed_reference).expect("valid cleanup fixture"),
            &session,
            None
        )
        .is_err());
        let duplicate = format!("{{\"settled\":true,{}", &bytes[1..]);
        assert!(SourceEndReceipt::parse(duplicate.as_bytes(), &session, None).is_err());
        assert!(
            SourceEndReceipt::parse(format!("{bytes} {{}}").as_bytes(), &session, None).is_err()
        );
        assert!(SourceEndReceipt::parse(&vec![b' '; MAX_REPLY + 1], &session, None).is_err());
        let mut pending = reply.clone();
        pending
            .as_object_mut()
            .expect("valid cleanup fixture")
            .remove("session_id");
        pending
            .as_object_mut()
            .expect("valid cleanup fixture")
            .remove("control_epoch");
        assert!(SourceEndReceipt::parse(
            &serde_json::to_vec(&pending).expect("valid cleanup fixture"),
            &session,
            None
        )
        .is_ok());
        assert!(SourceEndReceipt::parse(
            &serde_json::to_vec(&pending).expect("valid cleanup fixture"),
            &session,
            Some(&lineage)
        )
        .is_err());
        pending["session_id"] = json!(lineage.session_id);
        assert!(SourceEndReceipt::parse(
            &serde_json::to_vec(&pending).expect("valid cleanup fixture"),
            &session,
            None
        )
        .is_err());
    }
}

// Authenticated live metadata from the original retained Source session. This
// receipt is neither physical settlement nor a receiver publication grant.
pub(crate) struct SourceStatusReceipt {
    response: StartResponse,
}
impl SourceStatusReceipt {
    fn parse(
        bytes: &[u8],
        session: &SourcePeerSession,
        known: &SourcePeerLineage,
    ) -> Result<Self, PeerError> {
        let decoded = crate::http::decode_source_start_response(bytes, &session.reference)
            .map_err(|_| PeerError::InvalidResponse)?;
        let (_, incarnation, response) = decoded.into_parts();
        if &SourcePeerLineage::from_start(incarnation, &response)? != known
            || !response.vod
            || response.media_origin_ms != Some(0)
        {
            return Err(PeerError::InvalidResponse);
        }
        Ok(Self { response })
    }
    pub(crate) fn response(&self) -> &StartResponse {
        &self.response
    }
}
impl PeerConnection {
    // Dedicated fixed-path POST; a terminal cleanup envelope cannot satisfy it.
    pub(crate) async fn file_status(
        &mut self,
        credential: &Secret,
        viewer_hash: &str,
        session: &SourcePeerSession,
        known: &SourcePeerLineage,
    ) -> Result<SourceStatusReceipt, PeerError> {
        if self.verified_endpoint.is_none()
            || viewer_hash.len() != 64
            || !viewer_hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(PeerError::InvalidResponse);
        }
        let body = session.end_body(Some(known))?;
        tokio::time::timeout(Duration::from_secs(10), async {
            let mut auth = HeaderValue::from_str(&format!("CinemaShare {}", credential.expose()))
                .map_err(|_| PeerError::InvalidResponse)?;
            auth.set_sensitive(true);
            let mut viewer =
                HeaderValue::from_str(viewer_hash).map_err(|_| PeerError::InvalidResponse)?;
            viewer.set_sensitive(true);
            let request = Request::builder()
                .method(Method::POST)
                .uri(format!(
                    "/sharing/v1/items/{}/files/{}/sessions/{}/status",
                    session.reference.item_id.as_str(),
                    session.reference.file_id.as_str(),
                    session.request_id
                ))
                .header(header::HOST, &self.host)
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::ACCEPT, "application/json")
                .header(header::AUTHORIZATION, auth)
                .header("cinemashare-viewer", viewer)
                .body(Body::from(body))
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
            let bytes = axum::body::to_bytes(
                Body::new(
                    response
                        .into_body()
                        .map_err(|_| std::io::Error::other("sharing peer status body")),
                ),
                if status == StatusCode::OK {
                    4 * 1024 * 1024
                } else {
                    MAX_REPLY
                },
            )
            .await
            .map_err(|_| PeerError::InvalidResponse)?;
            if status == StatusCode::UNAUTHORIZED {
                return Err(PeerError::Authentication);
            }
            if status == StatusCode::UPGRADE_REQUIRED {
                return Err(PeerError::ProtocolUnsupported);
            }
            if status != StatusCode::OK {
                return Err(PeerError::Rejected(status));
            }
            SourceStatusReceipt::parse(&bytes, session, known)
        })
        .await
        .map_err(|_| PeerError::Unavailable)?
    }
}
#[cfg(test)]
mod status_tests {
    use super::*;
    fn fixture() -> (SourcePeerSession, SourcePeerLineage, Value) {
        let reference = SourcePlaybackTarget {
            server_id: Uuid::new_v4(),
            catalogue_epoch: Uuid::new_v4(),
            library_id: SourceId::parse("0").expect("library"),
            item_id: SourceId::parse("9007199254740993").expect("item"),
            file_id: SourceId::parse("0").expect("file"),
            revision: plurx_core::sharing_catalogue_details::FileRevision::parse(&"a".repeat(64))
                .expect("revision"),
        };
        let lineage = SourcePeerLineage {
            incarnation_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            control_epoch: 7,
        };
        let wrapper = json!({"reference":reference,"session":{"request_id":Uuid::new_v4(),"playback_id":"original","start":30.0,"copy":true}});
        let session = SourcePeerSession::new(
            reference.clone(),
            &serde_json::to_vec(&wrapper).expect("request"),
        )
        .expect("session");
        let mut response:StartResponse=serde_json::from_value(json!({
            "session_id":lineage.session_id,"playlist_url":format!("/api/v1/hls/{}/index.m3u8",lineage.session_id),
            "duration_ms":100000,"start_seconds":30.0,"media_origin_ms":0,"height":720,"encoder":"copy","vod":true,
            "ladder":[],"plan_notes":[]
        })).expect("actual response DTO");
        response.control = crate::playback_control::ControlBootstrap::new(
            &lineage.session_id.to_string(),
            &lineage.incarnation_id.to_string(),
            7,
            crate::playback_control::VOD_LEASE_TIMEOUT_MS,
        );
        let value = json!({"reference":reference,"incarnation_id":lineage.incarnation_id,"response":response});
        (session, lineage, value)
    }
    #[test]
    fn source_status_requires_current_complete_exact_lineage() {
        let (session, known, value) = fixture();
        let encode = |v: &Value| serde_json::to_vec(v).expect("wire response");
        let receipt =
            SourceStatusReceipt::parse(&encode(&value), &session, &known).expect("actual shape");
        assert_eq!(receipt.response().session_id, known.session_id.to_string());
        for key in ["incarnation_id", "session_id", "control_epoch"] {
            let mut foreign = known.clone();
            match key {
                "incarnation_id" => foreign.incarnation_id = Uuid::new_v4(),
                "session_id" => foreign.session_id = Uuid::new_v4(),
                _ => foreign.control_epoch += 1,
            }
            assert!(
                SourceStatusReceipt::parse(&encode(&value), &session, &foreign).is_err(),
                "{key}"
            );
        }
        let mut drift = value.clone();
        drift["reference"]["catalogue_epoch"] = json!(Uuid::new_v4());
        assert!(SourceStatusReceipt::parse(&encode(&drift), &session, &known).is_err());
        let mut drift = value.clone();
        drift["response"]["media_origin_ms"] = json!(1);
        assert!(SourceStatusReceipt::parse(&encode(&drift), &session, &known).is_err());
        let cleanup = json!({"reference":session.reference,"request_id":session.request_id,"incarnation_id":known.incarnation_id,"state":"settled"});
        assert!(SourceStatusReceipt::parse(&encode(&cleanup), &session, &known).is_err());
    }
}

/// Validated Source response facts. This value does not authorize a B reader;
/// that requires the original receiver login and its exact delivery grant.
pub(crate) struct SourcePeerResource {
    pub(crate) body: Body,
    pub(crate) length: u64,
    pub(crate) mime: &'static str,
    pub(crate) etag: Option<String>,
}
struct SourceResourceHead {
    length: u64,
    mime: &'static str,
    etag: Option<String>,
    playlist: bool,
}
fn single_header<'a>(headers: &'a axum::http::HeaderMap, name: &str) -> Result<&'a str, PeerError> {
    let mut values = headers.get_all(name).iter();
    let value = values.next().ok_or(PeerError::InvalidResponse)?;
    if values.next().is_some() {
        return Err(PeerError::InvalidResponse);
    }
    value.to_str().map_err(|_| PeerError::InvalidResponse)
}
impl SourceResourceHead {
    fn parse(
        headers: &axum::http::HeaderMap,
        session: &SourcePeerSession,
        known: &SourcePeerLineage,
        resource: &plurx_core::sharing_resources::SharingHlsResource,
    ) -> Result<Self, PeerError> {
        use plurx_core::sharing_resources::{
            SharingHlsResourceKind as Kind, MAX_SHARING_PLAYLIST_BYTES,
        };
        let expected = [
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
            ("cinemashare-resource", resource.as_str().to_owned()),
        ];
        for (name, value) in expected {
            if single_header(headers, name)? != value {
                return Err(PeerError::InvalidResponse);
            }
        }
        if headers.contains_key(header::TRANSFER_ENCODING)
            || headers.contains_key(header::CONTENT_ENCODING)
            || single_header(headers, "cache-control")? != "no-store"
        {
            return Err(PeerError::InvalidResponse);
        }
        let length_text = single_header(headers, "content-length")?;
        let length = length_text
            .parse::<u64>()
            .map_err(|_| PeerError::InvalidResponse)?;
        if length == 0 || length.to_string() != length_text {
            return Err(PeerError::InvalidResponse);
        }
        let (mime, maximum, playlist, file) = match resource.kind() {
            Kind::Master | Kind::Index | Kind::Video | Kind::SubtitlePlaylist { .. } => (
                "application/vnd.apple.mpegurl",
                MAX_SHARING_PLAYLIST_BYTES as u64,
                true,
                false,
            ),
            Kind::Init => ("video/mp4", 256 * 1024 * 1024, false, true),
            Kind::MediaSegment => ("video/iso.segment", 256 * 1024 * 1024, false, true),
            Kind::SubtitleSegment { .. } => ("text/vtt", 2 * 1024 * 1024, false, false),
        };
        if length > maximum || single_header(headers, "content-type")? != mime {
            return Err(PeerError::InvalidResponse);
        }
        let etag = if file {
            let value = single_header(headers, "etag")?;
            if value.is_empty()
                || value.len() > 512
                || !value.is_ascii()
                || value.bytes().any(|b| b <= b' ' || b == 127)
            {
                return Err(PeerError::InvalidResponse);
            }
            Some(value.to_owned())
        } else {
            if headers.contains_key("etag") {
                return Err(PeerError::InvalidResponse);
            }
            None
        };
        Ok(Self {
            length,
            mime,
            etag,
            playlist,
        })
    }
}
impl PeerConnection {
    pub(crate) async fn file_resource(
        mut self,
        credential: &Secret,
        viewer_hash: &str,
        session: &SourcePeerSession,
        known: &SourcePeerLineage,
        resource: &plurx_core::sharing_resources::SharingHlsResource,
    ) -> Result<SourcePeerResource, PeerError> {
        if self.verified_endpoint.is_none()
            || viewer_hash.len() != 64
            || !viewer_hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(PeerError::InvalidResponse);
        }
        let mut value: Value = serde_json::from_slice(&session.end_body(Some(known))?)
            .map_err(|_| PeerError::InvalidResponse)?;
        value["resource"] = json!(resource.as_str());
        let body = serde_json::to_vec(&value).map_err(|_| PeerError::InvalidResponse)?;
        if body.len() > 128 * 1024 {
            return Err(PeerError::InvalidResponse);
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        let response = tokio::time::timeout_at(deadline, async {
            let mut auth = HeaderValue::from_str(&format!("CinemaShare {}", credential.expose()))
                .map_err(|_| PeerError::InvalidResponse)?;
            auth.set_sensitive(true);
            let mut viewer =
                HeaderValue::from_str(viewer_hash).map_err(|_| PeerError::InvalidResponse)?;
            viewer.set_sensitive(true);
            let request = Request::builder()
                .method(Method::POST)
                .uri(format!(
                    "/sharing/v1/items/{}/files/{}/sessions/{}/resources",
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
        match response.status() {
            StatusCode::OK => {}
            StatusCode::UNAUTHORIZED => return Err(PeerError::Authentication),
            StatusCode::UPGRADE_REQUIRED => return Err(PeerError::ProtocolUnsupported),
            status => return Err(PeerError::Rejected(status)),
        }
        let head = SourceResourceHead::parse(response.headers(), session, known, resource)?;
        let body = if head.playlist {
            let bytes = tokio::time::timeout_at(
                deadline,
                axum::body::to_bytes(Body::new(response.into_body()), head.length as usize),
            )
            .await
            .map_err(|_| PeerError::Unavailable)?
            .map_err(|_| PeerError::InvalidResponse)?;
            if bytes.len() as u64 != head.length {
                return Err(PeerError::InvalidResponse);
            }
            plurx_core::sharing_resources::validate_sharing_playlist(resource, &bytes)
                .map_err(|_| PeerError::InvalidResponse)?;
            // Close and join the actual upstream socket before returning the
            // buffered playlist. Its EOF alone cannot release the task guard.
            self.connection.abort();
            let _ = (&mut self.connection).await;
            Body::from(bytes)
        } else {
            bounded_resource_body(self, Body::new(response.into_body()), head.length, deadline)
        };
        Ok(SourcePeerResource {
            body,
            length: head.length,
            mime: head.mime,
            etag: head.etag,
        })
    }
}

struct ResourceStreamState {
    _peer: PeerConnection,
    incoming: Body,
    remaining: u64,
    pending: axum::body::Bytes,
    deadline: tokio::time::Instant,
}
fn bounded_resource_body(
    peer: PeerConnection,
    incoming: Body,
    remaining: u64,
    deadline: tokio::time::Instant,
) -> Body {
    let stream = futures_util::stream::unfold(
        Some(ResourceStreamState {
            _peer: peer,
            incoming,
            remaining,
            pending: axum::body::Bytes::new(),
            deadline,
        }),
        |state| async move {
            let mut state = state?;
            loop {
                if tokio::time::Instant::now() >= state.deadline {
                    return Some((
                        Err(std::io::Error::other("sharing resource deadline")),
                        None,
                    ));
                }
                if !state.pending.is_empty() {
                    // Hyper may coalesce several original 64KiB Source writes.
                    // Split its frame without copying or buffering the whole file.
                    let bytes = state.pending.split_to(state.pending.len().min(64 * 1024));
                    return Some((Ok(bytes), Some(state)));
                }
                let frame = tokio::time::timeout_at(state.deadline, state.incoming.frame()).await;
                match frame {
                    Ok(Some(Ok(frame))) => {
                        let Ok(bytes) = frame.into_data() else {
                            return Some((
                                Err(std::io::Error::other("sharing resource trailers")),
                                None,
                            ));
                        };
                        if bytes.len() as u64 > state.remaining {
                            return Some((
                                Err(std::io::Error::other("sharing resource length")),
                                None,
                            ));
                        }
                        state.remaining -= bytes.len() as u64;
                        state.pending = bytes;
                    }
                    Ok(None) if state.remaining == 0 => return None,
                    _ => {
                        return Some((
                            Err(std::io::Error::other("sharing resource incomplete")),
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
mod resource_tests {
    use super::*;
    async fn wire_peer() -> (PeerConnection, tokio::io::DuplexStream) {
        let (client, server) = tokio::io::duplex(4096);
        (
            PeerConnection::from_stream(client, "body-framing-fixture".into())
                .await
                .expect("actual Hyper driver"),
            server,
        )
    }
    #[tokio::test]
    async fn source_resource_body_bounds_frames_bytes_and_original_deadline() {
        let data = vec![b'x'; 3 * 64 * 1024 + 17];
        let (peer, _server) = wire_peer().await;
        let mut body = bounded_resource_body(
            peer,
            Body::from(data.clone()),
            data.len() as u64,
            tokio::time::Instant::now() + Duration::from_secs(5),
        );
        let mut received = Vec::new();
        let mut frames = 0;
        while let Some(frame) = body.frame().await {
            let bytes = frame.expect("bounded body").into_data().expect("data");
            assert!(bytes.len() <= 64 * 1024);
            received.extend_from_slice(&bytes);
            frames += 1;
        }
        assert_eq!(received, data);
        assert_eq!(frames, 4);
        for expected in [data.len() as u64 - 1, data.len() as u64 + 1] {
            let (peer, _server) = wire_peer().await;
            let body = bounded_resource_body(
                peer,
                Body::from(data.clone()),
                expected,
                tokio::time::Instant::now() + Duration::from_secs(5),
            );
            assert!(
                body.collect().await.is_err(),
                "exact declared length {expected}"
            );
        }
        let (peer, _server) = wire_peer().await;
        let body = bounded_resource_body(
            peer,
            Body::from(data.clone()),
            data.len() as u64,
            tokio::time::Instant::now(),
        );
        assert!(
            body.collect().await.is_err(),
            "even immediately ready bytes cannot reset the original deadline"
        );
        let (peer, _server) = wire_peer().await;
        let frames =
            futures_util::stream::iter([Ok::<_, std::convert::Infallible>(hyper::body::Frame::<
                axum::body::Bytes,
            >::trailers(
                axum::http::HeaderMap::new(),
            ))]);
        let body = bounded_resource_body(
            peer,
            Body::new(http_body_util::StreamBody::new(frames)),
            0,
            tokio::time::Instant::now() + Duration::from_secs(5),
        );
        assert!(
            body.collect().await.is_err(),
            "closed resource representation has no trailers"
        );
    }
    fn fixture() -> (
        SourcePeerSession,
        SourcePeerLineage,
        plurx_core::sharing_resources::SharingHlsResource,
        axum::http::HeaderMap,
    ) {
        use plurx_core::{sharing::SourceId, sharing_catalogue_details::FileRevision};
        let reference = SourcePlaybackTarget {
            server_id: Uuid::new_v4(),
            catalogue_epoch: Uuid::new_v4(),
            library_id: SourceId::parse("1").expect("library"),
            item_id: SourceId::parse("9007199254740993").expect("item"),
            file_id: SourceId::parse("9223372036854775807").expect("file"),
            revision: FileRevision::parse(&"a".repeat(64)).expect("revision"),
        };
        let request_id = Uuid::new_v4();
        let session=SourcePeerSession::new(reference.clone(),&serde_json::to_vec(&json!({"reference":reference,"session":{"request_id":request_id,"playback_id":"original-player","quality":"1080p","start_seconds":0,"caps":{"hevc":false}}})).expect("request")).expect("session");
        let known = SourcePeerLineage {
            incarnation_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            control_epoch: 7,
        };
        let resource =
            plurx_core::sharing_resources::SharingHlsResource::parse("init.mp4").expect("resource");
        let mut headers = axum::http::HeaderMap::new();
        for (name, value) in [
            (
                "cinemashare-reference",
                serde_json::to_string(&reference).expect("reference"),
            ),
            ("cinemashare-request-id", request_id.to_string()),
            (
                "cinemashare-incarnation-id",
                known.incarnation_id.to_string(),
            ),
            ("cinemashare-session-id", known.session_id.to_string()),
            ("cinemashare-control-epoch", known.control_epoch.to_string()),
            ("cinemashare-resource", resource.as_str().to_owned()),
            ("content-length", "8192".into()),
            ("content-type", "video/mp4".into()),
            ("etag", "immutable-file-8192".into()),
            ("cache-control", "no-store".into()),
        ] {
            headers.insert(
                axum::http::HeaderName::from_bytes(name.as_bytes()).expect("name"),
                value.parse().expect("value"),
            );
        }
        (session, known, resource, headers)
    }
    #[test]
    fn source_resource_echo_requires_exact_lineage_single_headers_and_closed_representation() {
        let (session, known, resource, headers) = fixture();
        let parsed =
            SourceResourceHead::parse(&headers, &session, &known, &resource).expect("exact head");
        assert_eq!(parsed.length, 8192);
        for (name, value) in [
            ("cinemashare-request-id", Uuid::new_v4().to_string()),
            ("cinemashare-incarnation-id", Uuid::new_v4().to_string()),
            ("cinemashare-session-id", Uuid::new_v4().to_string()),
            ("cinemashare-control-epoch", "8".into()),
            ("cinemashare-resource", "seg00000.m4s".into()),
            ("content-length", "08192".into()),
            ("content-length", "0".into()),
            ("content-length", (256_u64 * 1024 * 1024 + 1).to_string()),
            ("content-type", "application/octet-stream".into()),
            ("cache-control", "public".into()),
            ("etag", "".into()),
        ] {
            let mut changed = headers.clone();
            changed.insert(
                axum::http::HeaderName::from_bytes(name.as_bytes()).expect("name"),
                value.parse().expect("value"),
            );
            assert!(
                SourceResourceHead::parse(&changed, &session, &known, &resource).is_err(),
                "{name}"
            );
        }
        for name in [
            "cinemashare-reference",
            "cinemashare-resource",
            "content-length",
            "content-type",
            "etag",
        ] {
            let mut changed = headers.clone();
            changed.append(
                axum::http::HeaderName::from_bytes(name.as_bytes()).expect("name"),
                headers[name].clone(),
            );
            assert!(
                SourceResourceHead::parse(&changed, &session, &known, &resource).is_err(),
                "duplicate {name}"
            );
        }
        for name in ["transfer-encoding", "content-encoding"] {
            let mut changed = headers.clone();
            changed.insert(
                axum::http::HeaderName::from_bytes(name.as_bytes()).expect("name"),
                "identity".parse().expect("value"),
            );
            assert!(
                SourceResourceHead::parse(&changed, &session, &known, &resource).is_err(),
                "{name}"
            );
        }
        let mut changed = headers;
        let mut reference = serde_json::to_value(&session.reference).expect("reference");
        reference["revision"] = json!("b".repeat(64));
        changed.insert(
            "cinemashare-reference",
            serde_json::to_string(&reference)
                .expect("reference")
                .parse()
                .expect("header"),
        );
        assert!(SourceResourceHead::parse(&changed, &session, &known, &resource).is_err());
    }
}

/// B's bounded, closed copy of one Source VOD observation. Text fields are
/// short machine vocabulary, never prose; numbers are the same bounds the
/// Source applied before publishing. Neither a Local session identity nor a
/// numeric Source file identity can be represented.
#[derive(Clone, Debug, PartialEq, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SharedVodStatus {
    pub(crate) active_encode_milli_realtime: Option<u32>,
    pub(crate) active_encode_age_ms: Option<u32>,
    pub(crate) active_encode_active_ms: Option<u32>,
    pub(crate) active_encode_segments: Option<u32>,
    pub(crate) active_encode_candidate_id: Option<plurx_core::playback::candidate::CandidateId>,
    pub(crate) target_height: i64,
    pub(crate) encoder: String,
    pub(crate) tone_map_peak_nits: Option<u32>,
    pub(crate) tone_map_peak_source: Option<String>,
    pub(crate) playlist_shape: String,
    pub(crate) producer_state: String,
    pub(crate) producer_hold: Option<String>,
    pub(crate) producer_decision: Option<String>,
    pub(crate) control_demand: Option<String>,
    pub(crate) reported_position_ms: Option<i64>,
    pub(crate) client_runway_ms: Option<i64>,
    pub(crate) render_state: Option<String>,
    pub(crate) server_ready_state: String,
    pub(crate) server_ready_anchor_ms: Option<i64>,
    pub(crate) server_ready_end_ms: Option<i64>,
    pub(crate) server_ready_seconds: Option<f64>,
    pub(crate) server_next_ready_start_ms: Option<i64>,
    pub(crate) server_next_ready_end_ms: Option<i64>,
    pub(crate) published_end_ms: Option<i64>,
    pub(crate) ready_ahead_end_ms: Option<i64>,
    pub(crate) fetched_end_ms: i64,
    pub(crate) fetched_segment: Option<i64>,
    pub(crate) ahead_seconds: Option<i64>,
    pub(crate) materialized_segments: u64,
    pub(crate) planned_segments: u64,
    pub(crate) materialized_bytes: u64,
    pub(crate) planned_bytes: u64,
    pub(crate) working_set_bytes: u64,
    pub(crate) working_set_budget_bytes: u64,
    pub(crate) completed_cache_bytes: u64,
    pub(crate) admitted: bool,
    pub(crate) delivered_bytes: i64,
    pub(crate) delivered_bps: Option<i64>,
    pub(crate) delivered_idle_ms: i64,
    pub(crate) http_wait_count: u64,
    pub(crate) http_wait_oldest_ms: Option<i64>,
    pub(crate) http_wait_segment: Option<i64>,
    pub(crate) status_generated_unix_ms: i64,
    pub(crate) suspended: bool,
    #[serde(rename = "final")]
    pub(crate) final_: bool,
}
fn status_token(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 32
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}
impl SharedVodStatus {
    pub(crate) fn is_valid(&self) -> bool {
        const MAX_SAFE: i64 = 9_007_199_254_740_991;
        let safe = |value: i64| (0..=MAX_SAFE).contains(&value);
        (1..=16_384).contains(&self.target_height)
            && self
                .server_ready_seconds
                .is_none_or(|value| value.is_finite() && value >= 0.0)
            && safe(self.delivered_bytes)
            && self.delivered_bps.is_none_or(safe)
            && safe(self.delivered_idle_ms)
            && safe(self.fetched_end_ms)
            && safe(self.status_generated_unix_ms)
            && [
                self.reported_position_ms,
                self.client_runway_ms,
                self.server_ready_anchor_ms,
                self.server_ready_end_ms,
                self.server_next_ready_start_ms,
                self.server_next_ready_end_ms,
                self.published_end_ms,
                self.ready_ahead_end_ms,
                self.fetched_segment,
                self.ahead_seconds,
                self.http_wait_oldest_ms,
                self.http_wait_segment,
            ]
            .into_iter()
            .flatten()
            .all(safe)
            && [
                Some(self.encoder.as_str()),
                Some(self.playlist_shape.as_str()),
                Some(self.producer_state.as_str()),
                self.producer_hold.as_deref(),
                self.producer_decision.as_deref(),
                self.control_demand.as_deref(),
                self.render_state.as_deref(),
                Some(self.server_ready_state.as_str()),
                self.tone_map_peak_source.as_deref(),
            ]
            .into_iter()
            .flatten()
            .all(status_token)
            && [
                self.materialized_segments,
                self.planned_segments,
                self.materialized_bytes,
                self.planned_bytes,
                self.working_set_bytes,
                self.working_set_budget_bytes,
                self.completed_cache_bytes,
                self.http_wait_count,
            ]
            .into_iter()
            .all(|value| value <= MAX_SAFE as u64)
    }
}

/// The exact echo every Source session operation reply must carry.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OperationEcho {
    reference: SourcePlaybackTarget,
    #[serde(deserialize_with = "canonical_uuid")]
    request_id: Uuid,
    #[serde(deserialize_with = "canonical_uuid")]
    incarnation_id: Uuid,
    #[serde(deserialize_with = "canonical_uuid")]
    session_id: Uuid,
    control_epoch: i64,
}
/// Split a bounded reply into its exact echo and its one payload member.
fn operation_reply(
    bytes: &[u8],
    session: &SourcePeerSession,
    known: &SourcePeerLineage,
    payloads: &[&str],
) -> Result<(String, Value), PeerError> {
    if bytes.is_empty() || bytes.len() > MAX_REPLY {
        return Err(PeerError::InvalidResponse);
    }
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    let value = crate::http::bounded_decision_value(&mut decoder)
        .map_err(|_| PeerError::InvalidResponse)?;
    decoder.end().map_err(|_| PeerError::InvalidResponse)?;
    let mut object = match value {
        Value::Object(object) => object,
        _ => return Err(PeerError::InvalidResponse),
    };
    let present: Vec<&str> = payloads
        .iter()
        .copied()
        .filter(|key| object.contains_key(*key))
        .collect();
    let [payload] = present.as_slice() else {
        return Err(PeerError::InvalidResponse);
    };
    let payload = (*payload).to_owned();
    let member = object.remove(&payload).ok_or(PeerError::InvalidResponse)?;
    let expected_reference =
        serde_json::to_value(&session.reference).map_err(|_| PeerError::InvalidResponse)?;
    if object.get("reference") != Some(&expected_reference)
        || object.get("session_id").and_then(Value::as_str)
            != Some(known.session_id.to_string().as_str())
        || object.get("incarnation_id").and_then(Value::as_str)
            != Some(known.incarnation_id.to_string().as_str())
    {
        return Err(PeerError::InvalidResponse);
    }
    let echo: OperationEcho =
        serde_json::from_value(Value::Object(object)).map_err(|_| PeerError::InvalidResponse)?;
    if echo.reference != session.reference
        || echo.request_id != session.request_id
        || echo.incarnation_id != known.incarnation_id
        || echo.session_id != known.session_id
        || echo.control_epoch != known.control_epoch
    {
        return Err(PeerError::InvalidResponse);
    }
    Ok((payload, member))
}

/// Authenticated live VOD metrics for the retained Source session. Neither a
/// readiness grant nor an activity renewal.
pub(crate) struct SourceVodStatusReceipt(SharedVodStatus);
impl SourceVodStatusReceipt {
    pub(crate) fn parse(
        bytes: &[u8],
        session: &SourcePeerSession,
        known: &SourcePeerLineage,
    ) -> Result<Self, PeerError> {
        let (_, status) = operation_reply(bytes, session, known, &["status"])?;
        let status: SharedVodStatus =
            serde_json::from_value(status).map_err(|_| PeerError::InvalidResponse)?;
        if !status.is_valid() {
            return Err(PeerError::InvalidResponse);
        }
        Ok(Self(status))
    }
    pub(crate) fn into_status(self) -> SharedVodStatus {
        self.0
    }
}

/// One Source control outcome, checked against the exact Source tuple and the
/// request that was actually sent. B rebinds it before any client sees it.
pub(crate) struct SourceControlReceipt(
    Result<
        crate::playback_control::ControlResponseV1,
        crate::http::sharing_playback_wire::SharedControlRefusal,
    >,
);
impl SourceControlReceipt {
    pub(crate) fn parse(
        bytes: &[u8],
        session: &SourcePeerSession,
        known: &SourcePeerLineage,
        sent: &crate::playback_control::ControlRequestV1,
    ) -> Result<Self, PeerError> {
        let (payload, member) = operation_reply(bytes, session, known, &["response", "refusal"])?;
        if payload == "refusal" {
            let refusal: crate::http::sharing_playback_wire::SharedControlRefusal =
                serde_json::from_value(member).map_err(|_| PeerError::InvalidResponse)?;
            if !refusal.is_valid() {
                return Err(PeerError::InvalidResponse);
            }
            return Ok(Self(Err(refusal)));
        }
        let response: crate::playback_control::ControlResponseV1 =
            serde_json::from_value(member).map_err(|_| PeerError::InvalidResponse)?;
        let relay = crate::playback_control::ControlRelayRequest {
            session_id: known.session_id.to_string(),
            generation: known.incarnation_id.to_string(),
            expected_owner_node_id: "source".to_owned(),
            expected_owner_epoch: known.control_epoch,
            deadline_unix_ms: 0,
            control: sent.clone(),
        };
        if sent.generation != relay.generation
            || i64::try_from(sent.control_epoch).ok() != Some(known.control_epoch)
            || !response.is_valid_for(&relay)
            || matches!(
                response.action,
                crate::playback_control::ControlAction::Prepare { .. }
            )
        {
            return Err(PeerError::InvalidResponse);
        }
        Ok(Self(Ok(response)))
    }
    pub(crate) fn into_outcome(
        self,
    ) -> Result<
        crate::playback_control::ControlResponseV1,
        crate::http::sharing_playback_wire::SharedControlRefusal,
    > {
        self.0
    }
}

impl SourcePeerSession {
    fn operation_body(
        &self,
        known: &SourcePeerLineage,
        control: Option<&crate::playback_control::ControlRequestV1>,
    ) -> Result<Vec<u8>, PeerError> {
        let mut value: Value = serde_json::from_slice(&self.end_body(Some(known))?)
            .map_err(|_| PeerError::InvalidResponse)?;
        if let Some(control) = control {
            value["control"] =
                serde_json::to_value(control).map_err(|_| PeerError::InvalidResponse)?;
        }
        let bytes = serde_json::to_vec(&value).map_err(|_| PeerError::InvalidResponse)?;
        if bytes.len() > 128 * 1024 {
            return Err(PeerError::InvalidResponse);
        }
        Ok(bytes)
    }
}

impl PeerConnection {
    /// One fixed-path authenticated POST with a bounded reply. The caller owns
    /// the connection lifetime; a timeout never synthesizes a reply.
    async fn session_operation(
        &mut self,
        credential: &Secret,
        viewer_hash: &str,
        session: &SourcePeerSession,
        operation: &'static str,
        body: Vec<u8>,
        budget: Duration,
    ) -> Result<Vec<u8>, PeerError> {
        if self.verified_endpoint.is_none()
            || viewer_hash.len() != 64
            || !viewer_hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(PeerError::InvalidResponse);
        }
        tokio::time::timeout(budget, async {
            let mut auth = HeaderValue::from_str(&format!("CinemaShare {}", credential.expose()))
                .map_err(|_| PeerError::InvalidResponse)?;
            auth.set_sensitive(true);
            let mut viewer =
                HeaderValue::from_str(viewer_hash).map_err(|_| PeerError::InvalidResponse)?;
            viewer.set_sensitive(true);
            let request = Request::builder()
                .method(Method::POST)
                .uri(format!(
                    "/sharing/v1/items/{}/files/{}/sessions/{}/{operation}",
                    session.reference.item_id.as_str(),
                    session.reference.file_id.as_str(),
                    session.request_id
                ))
                .header(header::HOST, &self.host)
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::ACCEPT, "application/json")
                .header(header::AUTHORIZATION, auth)
                .header("cinemashare-viewer", viewer)
                .body(Body::from(body))
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
            let bytes = axum::body::to_bytes(
                Body::new(
                    response
                        .into_body()
                        .map_err(|_| std::io::Error::other("sharing peer operation body")),
                ),
                MAX_REPLY,
            )
            .await
            .map_err(|_| PeerError::InvalidResponse)?;
            if status == StatusCode::UNAUTHORIZED {
                return Err(PeerError::Authentication);
            }
            if status == StatusCode::UPGRADE_REQUIRED {
                return Err(PeerError::ProtocolUnsupported);
            }
            if status != StatusCode::OK {
                return Err(PeerError::Rejected(status));
            }
            Ok(bytes.to_vec())
        })
        .await
        .map_err(|_| PeerError::Unavailable)?
    }
    pub(crate) async fn file_vod_status(
        &mut self,
        credential: &Secret,
        viewer_hash: &str,
        session: &SourcePeerSession,
        known: &SourcePeerLineage,
    ) -> Result<SourceVodStatusReceipt, PeerError> {
        let body = session.operation_body(known, None)?;
        let bytes = self
            .session_operation(
                credential,
                viewer_hash,
                session,
                "vod-status",
                body,
                Duration::from_secs(10),
            )
            .await?;
        SourceVodStatusReceipt::parse(&bytes, session, known)
    }
    pub(crate) async fn file_control(
        &mut self,
        credential: &Secret,
        viewer_hash: &str,
        session: &SourcePeerSession,
        known: &SourcePeerLineage,
        control: &crate::playback_control::ControlRequestV1,
    ) -> Result<SourceControlReceipt, PeerError> {
        let body = session.operation_body(known, Some(control))?;
        let bytes = self
            .session_operation(
                credential,
                viewer_hash,
                session,
                "control",
                body,
                Duration::from_millis(3_500),
            )
            .await?;
        SourceControlReceipt::parse(&bytes, session, known, control)
    }
}
