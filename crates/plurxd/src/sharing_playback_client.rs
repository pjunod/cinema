//! Closed Source session RPC data. Constructors convey no producer authority.
use super::*;
use crate::http::hls::{SourcePlaybackTarget, StartResponse};
use serde_json::{json, Value};

const MAX_REPLY: usize = 16 * 1024;
const MAX_EPOCH: i64 = 9_007_199_254_740_991;

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
        tokio::time::timeout(Duration::from_secs(35), async {
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
