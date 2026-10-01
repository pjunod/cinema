//! Independent cancellation negotiation. Legacy control JSON remains unchanged.
use super::*;

pub(crate) const QUALITY_CONTROL_PATH: &str = "/internal/cluster/media/sessions/quality-control";
pub(crate) const QUALITY_CONTROL_MAX_BYTES: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct QualityIntentIdentity {
    pub generation: String,
    pub control_epoch: u64,
    pub client_instance_id: String,
    pub lifetime_id: String,
    pub recipe_revision: u64,
    pub accepted_sequence: u64,
}

pub(super) fn quality_cancellation_receipt_key(identity: &QualityIntentIdentity) -> String {
    use sha2::{Digest, Sha256};
    let bytes = serde_json::to_vec(identity).expect("bounded quality identity serializes");
    format!("{:x}", Sha256::digest(bytes))
}

impl QualityIntentIdentity {
    pub(super) fn from_control(
        request: &crate::playback_control::ControlRequestV1,
    ) -> Option<Self> {
        let intent = request.intent.as_ref()?;
        Some(Self {
            generation: request.generation.clone(),
            control_epoch: request.control_epoch,
            client_instance_id: request.client_instance_id.clone(),
            lifetime_id: intent.lifetime_id.clone(),
            recipe_revision: intent.recipe_revision,
            accepted_sequence: request.sequence,
        })
    }

    fn valid(&self) -> bool {
        uuid::Uuid::parse_str(&self.generation).is_ok()
            && uuid::Uuid::parse_str(&self.client_instance_id).is_ok()
            && self.control_epoch > 0
            && (1..=9_007_199_254_740_991).contains(&self.recipe_revision)
            && (1..=9_007_199_254_740_991).contains(&self.accepted_sequence)
            && !self.lifetime_id.is_empty()
            && self.lifetime_id.len() <= plurx_core::playback::MAX_LIFETIME_ID
            && !self.lifetime_id.bytes().any(|b| b.is_ascii_control())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum QualityControlOperation {
    Discover,
    CancelUnappended,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct QualityControlRequest {
    pub version: u8,
    pub generation: String,
    pub control_epoch: u64,
    pub operation: QualityControlOperation,
    pub identity: Option<QualityIntentIdentity>,
}

impl QualityControlRequest {
    pub(crate) fn valid(&self) -> bool {
        self.version == 1
            && uuid::Uuid::parse_str(&self.generation).is_ok()
            && self.control_epoch > 0
            && match self.operation {
                QualityControlOperation::Discover => self.identity.is_none(),
                QualityControlOperation::CancelUnappended => {
                    self.identity.as_ref().is_some_and(|id| {
                        id.valid()
                            && id.generation == self.generation
                            && id.control_epoch == self.control_epoch
                    })
                }
            }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct QualityControlRelayRequest {
    pub session_id: String,
    pub expected_owner_node_id: String,
    pub deadline_unix_ms: i64,
    pub request: QualityControlRequest,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct QualityControlResponse {
    pub version: u8,
    pub generation: String,
    pub control_epoch: u64,
    pub features: Vec<String>,
    pub outcome: String,
    pub pending_identity: Option<QualityIntentIdentity>,
}

impl QualityControlResponse {
    pub(crate) fn valid_for(&self, request: &QualityControlRequest) -> bool {
        self.version == 1
            && self.generation == request.generation
            && self.control_epoch == request.control_epoch
            && self
                .features
                .iter()
                .all(|feature| feature == "quality_cancel_v1")
            && self.pending_identity.as_ref().is_none_or(|identity| {
                identity.valid()
                    && identity.generation == request.generation
                    && identity.control_epoch == request.control_epoch
            })
            && self.features.len() <= 1
            && match request.operation {
                QualityControlOperation::Discover => {
                    matches!(self.outcome.as_str(), "supported" | "unsupported")
                }
                QualityControlOperation::CancelUnappended => matches!(
                    self.outcome.as_str(),
                    "cancel_requested" | "cancelled" | "observation_unknown" | "unsupported"
                ),
            }
            && if self.outcome == "unsupported" {
                self.features.is_empty()
            } else {
                self.features == ["quality_cancel_v1"]
            }
    }
}

fn answer(
    request: &QualityControlRequest,
    outcome: &str,
    supported: bool,
    pending_identity: Option<QualityIntentIdentity>,
) -> Response {
    let reply = QualityControlResponse {
        version: 1,
        generation: request.generation.clone(),
        control_epoch: request.control_epoch,
        features: if supported {
            vec!["quality_cancel_v1".to_owned()]
        } else {
            vec![]
        },
        outcome: outcome.to_owned(),
        pending_identity,
    };
    let mut response = Json(reply).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}

/// A separate bounded envelope cannot reach an older strict v1 control parser.
/// An old ingress has no route; an old owner has no exact-auth peer endpoint.
pub async fn quality_control(
    State(state): State<AppState>,
    AxPath(session): AxPath<String>,
    body: Bytes,
) -> Response {
    let Ok(request) = serde_json::from_slice::<QualityControlRequest>(&body) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    if !request.valid() || uuid::Uuid::parse_str(&session).is_err() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let deadline_unix_ms = unix_ms().saturating_add(4000);
    match tokio::time::timeout(
        crate::playback_control::EXCHANGE_DEADLINE,
        quality_control_routed(&state, &session, request, None, deadline_unix_ms),
    )
    .await
    {
        Ok(response) => response,
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub(crate) async fn quality_control_routed(
    state: &AppState,
    session: &str,
    request: QualityControlRequest,
    expected_owner: Option<&str>,
    deadline_unix_ms: i64,
) -> Response {
    if !request.valid() || uuid::Uuid::parse_str(session).is_err() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let route = match state.media_sessions.control_route(session).await {
        Ok(Some(route)) => route,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    if route.incarnation_id != request.generation
        || u64::try_from(route.owner_epoch).ok() != Some(request.control_epoch)
        || expected_owner
            .is_some_and(|owner| owner != route.owner_node_id || owner != state.node_id)
    {
        return StatusCode::CONFLICT.into_response();
    }
    if route.state != "active" || route.lease_expires_at_ms <= unix_ms() {
        return StatusCode::GONE.into_response();
    }
    if let Some(refusal) = library_channel_control_refusal(state, &route).await {
        return refusal;
    }
    if let Some(refusal) = control_owner_refusal(&route, Some(request.control_epoch)) {
        return refusal;
    }
    if control_start_response(&route).is_none() {
        return answer(&request, "unsupported", false, None);
    }
    if let Err(retry_after_ms) = state.media_sessions.admit_control(session) {
        return control_error(
            StatusCode::TOO_MANY_REQUESTS,
            "control_rate_limited",
            "the quality control budget is exhausted",
            None,
            None,
            Some(retry_after_ms),
            None,
        );
    }
    if route.owner_node_id != state.node_id {
        let relay = QualityControlRelayRequest {
            session_id: session.to_owned(),
            expected_owner_node_id: route.owner_node_id.clone(),
            deadline_unix_ms,
            request: request.clone(),
        };
        return match state
            .media_sessions
            .quality_control(&route.owner_node_id, &relay)
            .await
        {
            Ok(Some(reply)) => {
                let mut response = Json(reply).into_response();
                response.headers_mut().insert(
                    header::CACHE_CONTROL,
                    axum::http::HeaderValue::from_static("no-store"),
                );
                response
            }
            Ok(None) => answer(&request, "unsupported", false, None),
            Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        };
    }
    match request.operation {
        QualityControlOperation::Discover => {
            let pending = quality_preparation_identity(&route.playback_id).filter(|identity| {
                identity.generation == request.generation
                    && identity.control_epoch == request.control_epoch
            });
            answer(&request, "supported", true, pending)
        }
        QualityControlOperation::CancelUnappended => {
            let identity = request
                .identity
                .as_ref()
                .expect("validated cancellation identity");
            let receipt_key = quality_cancellation_receipt_key(identity);
            let existing = match state.store.quality_cancellation_receipt(&receipt_key).await {
                Ok(receipt) => receipt,
                Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
            };
            if let Some(receipt) = existing {
                if receipt.state == "settled" {
                    return answer(&request, "cancelled", true, Some(identity.clone()));
                }
            } else {
                // Do not fill durable bounds for unknown or newer work.
                if quality_preparation_identity(&route.playback_id).as_ref() != Some(identity) {
                    return answer(
                        &request,
                        "observation_unknown",
                        true,
                        Some(identity.clone()),
                    );
                }
                let now = unix_ms();
                let receipt = plurx_core::store::QualityCancellationReceipt {
                    receipt_key,
                    generation: route.incarnation_id.clone(),
                    session_id: route.session_id.clone(),
                    owner_node_id: route.owner_node_id.clone(),
                    owner_epoch: route.owner_epoch,
                    client_instance_id: identity.client_instance_id.clone(),
                    lifetime_id: identity.lifetime_id.clone(),
                    recipe_revision: i64::try_from(identity.recipe_revision)
                        .expect("validated recipe revision"),
                    accepted_sequence: i64::try_from(identity.accepted_sequence)
                        .expect("validated control sequence"),
                    state: "requested".to_owned(),
                    created_at_ms: now,
                    updated_at_ms: now,
                };
                match state.store.request_quality_cancellation(&receipt).await {
                    Ok(Some(_)) => {}
                    Ok(None) | Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
                }
            }
            let found = cancel_quality_preparation(&route.playback_id, identity);
            // A detached abort is not proof of completed cleanup. Absence may
            // also mean commit already owns the successor; never claim retention.
            answer(
                &request,
                if found {
                    "cancel_requested"
                } else {
                    "observation_unknown"
                },
                true,
                Some(identity.clone()),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn discovery() -> QualityControlRequest {
        QualityControlRequest {
            version: 1,
            generation: uuid::Uuid::new_v4().to_string(),
            control_epoch: 1,
            operation: QualityControlOperation::Discover,
            identity: None,
        }
    }

    #[test]
    fn discovery_is_separate_strict_and_epoch_bound() {
        let request = discovery();
        assert!(request.valid());
        let mut wire = serde_json::to_value(&request).expect("serializable discovery");
        wire["unknown_extension"] = serde_json::json!(true);
        assert!(serde_json::from_value::<QualityControlRequest>(wire).is_err());
        let mut response = QualityControlResponse {
            version: 1,
            generation: request.generation.clone(),
            control_epoch: 1,
            features: vec!["quality_cancel_v1".into()],
            outcome: "supported".into(),
            pending_identity: None,
        };
        assert!(response.valid_for(&request));
        response.control_epoch = 2;
        assert!(!response.valid_for(&request));
        response.control_epoch = 1;
        response.features.push("continuous_quality_v1".into());
        assert!(!response.valid_for(&request));
    }

    #[test]
    fn cancellation_requires_an_exact_bounded_identity() {
        let mut request = discovery();
        request.operation = QualityControlOperation::CancelUnappended;
        assert!(!request.valid());
        request.identity = Some(QualityIntentIdentity {
            generation: request.generation.clone(),
            control_epoch: 1,
            client_instance_id: uuid::Uuid::new_v4().to_string(),
            lifetime_id: "film".into(),
            recipe_revision: 1,
            accepted_sequence: 3,
        });
        assert!(request.valid());
        request
            .identity
            .as_mut()
            .expect("installed cancellation identity")
            .control_epoch = 2;
        assert!(!request.valid());
        request
            .identity
            .as_mut()
            .expect("installed cancellation identity")
            .control_epoch = 1;
        request
            .identity
            .as_mut()
            .expect("installed cancellation identity")
            .accepted_sequence = 0;
        assert!(!request.valid());
    }
}
